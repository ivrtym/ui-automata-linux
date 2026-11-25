use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

#[derive(Debug, Clone)]
pub struct HttpSecurityConfig {
    pub auth_token: Option<String>,
    pub allowed_hosts: Vec<String>,
}

impl HttpSecurityConfig {
    pub fn from_cli_and_env(
        bind_host: &str,
        cli_token: Option<String>,
        cli_allowed_hosts: Vec<String>,
    ) -> anyhow::Result<Self> {
        let token = resolve_auth_token(cli_token)?;

        let mut hosts: Vec<String> = vec![
            "127.0.0.1".into(),
            "localhost".into(),
            "[::1]".into(),
            "::1".into(),
        ];
        for h in cli_allowed_hosts {
            let norm = extract_host_only(&h);
            if !norm.is_empty() && !hosts.contains(&norm) {
                hosts.push(norm);
            }
        }
        let norm_bind = extract_host_only(bind_host);
        if !is_wildcard_bind(&norm_bind) && !norm_bind.is_empty() && !hosts.contains(&norm_bind) {
            hosts.push(norm_bind.clone());
        }

        if !is_loopback_bind(bind_host) && token.is_none() {
            anyhow::bail!(
                "refusing to bind non-loopback host '{bind_host}' without --auth-token or AUTOMATA_MCP_TOKEN(_FILE)"
            );
        }

        Ok(Self {
            auth_token: token,
            allowed_hosts: hosts,
        })
    }
}

pub fn is_loopback_bind(host: &str) -> bool {
    matches!(
        extract_host_only(host).as_str(),
        "127.0.0.1" | "localhost" | "::1" | "[::1]"
    )
}

fn is_wildcard_bind(host: &str) -> bool {
    matches!(host, "0.0.0.0" | "::" | "[::]")
}

fn resolve_auth_token(cli_token: Option<String>) -> anyhow::Result<Option<String>> {
    if let Some(t) = cli_token.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Ok(Some(t));
    }
    if let Ok(env_t) = std::env::var("AUTOMATA_MCP_TOKEN") {
        let trimmed = env_t.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
    if let Ok(path) = std::env::var("AUTOMATA_MCP_TOKEN_FILE") {
        let trimmed_path = path.trim();
        if !trimmed_path.is_empty() {
            let content = std::fs::read_to_string(trimmed_path).map_err(|e| {
                anyhow::anyhow!("failed to read AUTOMATA_MCP_TOKEN_FILE '{trimmed_path}': {e}")
            })?;
            let token = content.trim().to_string();
            if token.is_empty() {
                anyhow::bail!("AUTOMATA_MCP_TOKEN_FILE '{trimmed_path}' is empty");
            }
            return Ok(Some(token));
        }
    }
    Ok(None)
}

pub fn extract_host_only(raw_host: &str) -> String {
    let trimmed = raw_host.trim().to_ascii_lowercase();
    if trimmed.starts_with('[') {
        if let Some(end) = trimmed.find(']') {
            return trimmed[..=end].to_string();
        }
        return trimmed;
    }
    if trimmed.matches(':').count() == 1 {
        if let Some((host, _port)) = trimmed.rsplit_once(':') {
            return host.to_string();
        }
    }
    trimmed
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn authorize_bearer(headers: &HeaderMap, expected: &str) -> bool {
    let Some(val) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(raw) = val.to_str() else {
        return false;
    };
    let Some(rest) = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
    else {
        return false;
    };
    ct_eq(rest.trim().as_bytes(), expected.as_bytes())
}

pub async fn guard_http_request(
    State(cfg): State<Arc<HttpSecurityConfig>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let headers = req.headers();
    if headers.contains_key(axum::http::header::ORIGIN) {
        return (
            StatusCode::FORBIDDEN,
            "Origin header rejected (DNS-rebinding protection)",
        )
            .into_response();
    }

    let host_ok = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| {
            let clean = extract_host_only(h);
            cfg.allowed_hosts.iter().any(|allowed| allowed == &clean)
        })
        .unwrap_or(false);
    if !host_ok {
        return (StatusCode::FORBIDDEN, "Host header not in allowed_hosts").into_response();
    }

    if let Some(ref expected) = cfg.auth_token {
        if !authorize_bearer(headers, expected) {
            return (
                StatusCode::UNAUTHORIZED,
                "missing or invalid Bearer authorization token",
            )
                .into_response();
        }
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::{HttpSecurityConfig, extract_host_only, is_loopback_bind};

    #[test]
    fn normalizes_hosts_and_detects_loopback() {
        assert_eq!(extract_host_only("127.0.0.1:3742"), "127.0.0.1");
        assert_eq!(extract_host_only("[::1]:3742"), "[::1]");
        assert_eq!(extract_host_only("LocalHost:8080"), "localhost");
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("::1"));
        assert!(!is_loopback_bind("0.0.0.0"));
    }

    #[test]
    fn non_loopback_bind_requires_token() {
        let err = HttpSecurityConfig::from_cli_and_env("0.0.0.0", None, vec![]).unwrap_err();
        assert!(err.to_string().contains("--auth-token"));

        let ok =
            HttpSecurityConfig::from_cli_and_env("0.0.0.0", Some("secret".into()), vec![]).unwrap();
        assert_eq!(ok.auth_token.as_deref(), Some("secret"));
    }
}
