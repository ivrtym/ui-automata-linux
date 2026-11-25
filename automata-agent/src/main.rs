mod http_security;
mod server;
mod state;
mod tools;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use automata_linux::ConnectionMode;
use axum::{Router, middleware};
use clap::Parser;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
};

use crate::http_security::{HttpSecurityConfig, guard_http_request, is_loopback_bind};
use crate::server::AutomataMcpServer;
use crate::state::AgentState;

#[derive(Parser, Debug)]
#[command(name = "automata-agent", about = "UI-Automata Linux MCP server")]
struct Cli {
    /// Serve MCP over stdio JSON-RPC (no logging to stdout).
    #[arg(long)]
    stdio: bool,

    /// HTTP listen address when not running in --stdio mode.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// HTTP listen port when not running in --stdio mode.
    #[arg(long, default_value_t = 3742)]
    port: u16,

    /// Optional workflows directory exposed via `workflow(list)` and `resources`.
    #[arg(long)]
    workflows_dir: Option<PathBuf>,

    /// Connection lifecycle mode (`auto`, `oneshot`, `persistent`).
    /// Defaults to `persistent` for the long-running MCP server.
    #[arg(long, value_name = "MODE")]
    connection_mode: Option<String>,

    /// Bearer token required on HTTP `/mcp` requests (also reads AUTOMATA_MCP_TOKEN / AUTOMATA_MCP_TOKEN_FILE).
    #[arg(long, value_name = "TOKEN")]
    auth_token: Option<String>,

    /// Additional allowed HTTP `Host` header hostnames for `/mcp` (loopback hosts are always allowed).
    #[arg(long = "allowed-host", value_name = "HOST")]
    allowed_hosts: Vec<String>,

    /// Explicitly allow file/screenshot writes to host paths when binding non-loopback HTTP.
    #[arg(long)]
    allow_remote_file_writes: bool,
}

fn init_agent_logging(stdio: bool) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let log_dir = PathBuf::from(home).join(".ui-automata").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("automata-agent.log");

    if stdio {
        // Stdio mode: stdout is reserved exclusively for MCP JSON-RPC frames.
        automata_linux::init_logging_file_only(&log_path);
    } else {
        automata_linux::init_logging_file_only(&log_path);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_agent_logging(cli.stdio);

    if let Some(ref mode_arg) = cli.connection_mode {
        automata_linux::apply_connection_mode_arg(Some(mode_arg.as_str()))
            .map_err(|e| anyhow::anyhow!("invalid --connection-mode: {e}"))?;
    }

    let allow_file_writes = cli.stdio || is_loopback_bind(&cli.host) || cli.allow_remote_file_writes;

    if cli.stdio {
        let state = AgentState::with_file_policy(
            ConnectionMode::persistent_default(),
            cli.workflows_dir,
            allow_file_writes,
        );
        let server = AutomataMcpServer::new(state);
        let running = rmcp::serve_server(server, rmcp::transport::stdio()).await?;
        let _reason = running.waiting().await?;
    } else {
        let sec = Arc::new(HttpSecurityConfig::from_cli_and_env(
            &cli.host,
            cli.auth_token,
            cli.allowed_hosts,
        )?);
        let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
        let state = AgentState::with_file_policy(
            ConnectionMode::persistent_default(),
            cli.workflows_dir,
            allow_file_writes,
        );
        let server = AutomataMcpServer::new(state);
        let server_template = server.clone();
        let http_service = StreamableHttpService::new(
            move || Ok(server_template.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default(),
        );
        let app = Router::new()
            .nest_service("/mcp", http_service)
            .layer(middleware::from_fn_with_state(sec, guard_http_request));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!("automata-agent MCP HTTP listening on http://{addr}/mcp");
        axum::serve(listener, app).await?;
    }

    Ok(())
}
