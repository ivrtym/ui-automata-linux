//! Safe AT-SPI `EditableText` and `Value` mutation with GTK (Unicode scalar)
//! and Qt/Chromium (UTF-16 code unit) readback verification plus password-field
//! protection.

use std::time::{Duration, Instant};

use atspi::proxy::accessible::AccessibleProxy;
use atspi::proxy::proxy_ext::{Proxies, ProxyExt};
use atspi::proxy::text::TextProxy;
use atspi::{Interface, Role, State};
use tokio::time::sleep_until;

use crate::UiError;
use crate::a11y::client::A11yClient;

const EDITABLE_VERIFY_TIMEOUT: Duration = Duration::from_millis(450);
const EDITABLE_VERIFY_INTERVAL: Duration = Duration::from_millis(25);
const EDITABLE_READ_CHUNK_UNITS: usize = 1024;

/// Structured outcome of setting an element's value via AT-SPI2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableSetOutcome {
    /// Text contents were written and verified via UTF-8 or UTF-16 AT-SPI readback.
    Verified,
    /// Target is a `PasswordText` control; written without attempting readback.
    PasswordUnverifiable,
    /// Numeric value set through the AT-SPI `Value` interface.
    NumericValueSet,
    /// Readback timed out or failed (write call itself succeeded).
    ReadbackUnavailable,
    /// Readback contents differed from the requested string after the grace window
    /// (can occur on auto-formatting inputs; caller decides whether to warn).
    MismatchAfterDeadline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadbackError {
    TimedOut,
    Failed,
}

/// Validates that `text` contains no unsupported control characters before
/// calling `EditableText::set_text_contents`.
pub fn validate_editable_input(text: &str, multi_line: bool) -> Result<(), UiError> {
    if let Some(bad) = text
        .chars()
        .find(|ch| ch.is_control() && !(multi_line && (*ch == '\n' || *ch == '\t')))
    {
        return Err(UiError::Platform(format!(
            "refusing control character U+{:04X} for AT-SPI EditableText",
            bad as u32
        )));
    }
    Ok(())
}

/// Returns a safe upper chunk index in UTF-16 slice `units` that will not bisect
/// a UTF-16 surrogate pair (`0xD800..=0xDBFF`).
pub fn utf16_chunk_end(units: &[u16], start: usize, max_chunk: usize) -> usize {
    let mut end = start.saturating_add(max_chunk).min(units.len());
    if end > start && end < units.len() && (0xD800..=0xDBFF).contains(&units[end - 1]) {
        end -= 1;
    }
    end
}

pub fn is_password_role(role: Role, raw_role_name: &str) -> bool {
    role == Role::PasswordText || raw_role_name.eq_ignore_ascii_case("password text")
}

/// Sets an accessible node's text or numeric value via AT-SPI `EditableText` or
/// `Value`, verifying readback cleanly across GTK (scalar) and Qt/Chromium (UTF-16)
/// coordinate systems while protecting password fields.
pub fn set_editable_or_numeric_value(
    client: &A11yClient,
    bus_name: &str,
    path: &str,
    value: &str,
) -> Result<EditableSetOutcome, UiError> {
    client.block_on(async {
        let proxy = client.accessible_proxy(bus_name, path).await?;
        let ifaces = proxy
            .get_interfaces()
            .await
            .map_err(|e| UiError::Platform(e.to_string()))?;
        let proxies = proxy
            .proxies()
            .await
            .map_err(|e| UiError::Platform(e.to_string()))?;

        if ifaces.contains(Interface::EditableText) {
            let states = proxy.get_state().await.unwrap_or_default();
            let multi_line = states.contains(State::MultiLine);
            validate_editable_input(value, multi_line)?;

            let role = proxy.get_role().await.unwrap_or(Role::Invalid);
            let raw_role = if role == Role::Invalid {
                proxy.get_role_name().await.unwrap_or_default()
            } else {
                String::new()
            };

            let et = proxies
                .editable_text()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let ok = et
                .set_text_contents(value)
                .await
                .map_err(|e| UiError::Platform(format!("set_text_contents failed: {e}")))?;
            if !ok {
                return Err(UiError::Platform("set_text_contents returned false".into()));
            }

            if is_password_role(role, &raw_role) {
                return Ok(EditableSetOutcome::PasswordUnverifiable);
            }

            let outcome = verify_editable_text_contents(&proxy, &proxies, value).await;
            if outcome == EditableSetOutcome::MismatchAfterDeadline {
                log::debug!(
                    "EditableText readback mismatch after deadline on ({bus_name}, {path}); widget may have reformatted input"
                );
            }
            return Ok(outcome);
        }

        if ifaces.contains(Interface::Value) {
            if let Ok(num) = value.trim().parse::<f64>() {
                let vp = proxies
                    .value()
                    .await
                    .map_err(|e| UiError::Platform(e.to_string()))?;
                vp.set_current_value(num)
                    .await
                    .map_err(|e| UiError::Platform(format!("set_current_value failed: {e}")))?;
                return Ok(EditableSetOutcome::NumericValueSet);
            }
        }

        Err(UiError::Platform(
            "element does not support EditableText or numeric Value interface".into(),
        ))
    })
}

async fn readback_within<T, E>(
    deadline: Instant,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, ReadbackError> {
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), fut)
        .await
        .map_err(|_| ReadbackError::TimedOut)?
        .map_err(|_| ReadbackError::Failed)
}

async fn verify_editable_text_contents(
    proxy: &AccessibleProxy<'_>,
    proxies: &Proxies<'_>,
    requested: &str,
) -> EditableSetOutcome {
    let deadline = Instant::now() + EDITABLE_VERIFY_TIMEOUT;
    if matches!(
        readback_within(deadline, proxy.get_role()).await,
        Ok(Role::PasswordText)
    ) {
        return EditableSetOutcome::PasswordUnverifiable;
    }

    let Ok(text) = readback_within(deadline, proxies.text()).await else {
        return EditableSetOutcome::ReadbackUnavailable;
    };

    loop {
        match text_readback_matches(&text, requested, deadline).await {
            Ok(true) => return EditableSetOutcome::Verified,
            Ok(false) => {}
            Err(ReadbackError::TimedOut | ReadbackError::Failed) => {
                return EditableSetOutcome::ReadbackUnavailable;
            }
        }

        let now = Instant::now();
        if now >= deadline {
            return EditableSetOutcome::MismatchAfterDeadline;
        }
        let next = (now + EDITABLE_VERIFY_INTERVAL).min(deadline);
        sleep_until(tokio::time::Instant::from_std(next)).await;
    }
}

async fn text_readback_matches(
    text: &TextProxy<'_>,
    requested: &str,
    deadline: Instant,
) -> Result<bool, ReadbackError> {
    let char_count = readback_within(deadline, text.character_count()).await?;
    if char_count < 0 {
        return Err(ReadbackError::Failed);
    }

    if char_count as usize == requested.chars().count() {
        if scalar_readback_matches(text, requested, deadline).await? {
            return Ok(true);
        }
    }

    let utf16: Vec<u16> = requested.encode_utf16().collect();
    if char_count as usize != utf16.len() {
        return Ok(false);
    }
    utf16_readback_matches(text, &utf16, deadline).await
}

async fn scalar_readback_matches(
    text: &TextProxy<'_>,
    requested: &str,
    deadline: Instant,
) -> Result<bool, ReadbackError> {
    let chars: Vec<char> = requested.chars().collect();
    let mut offset = 0;
    while offset < chars.len() {
        let end = (offset + EDITABLE_READ_CHUNK_UNITS).min(chars.len());
        let chunk = readback_within(deadline, text.get_text(offset as i32, end as i32)).await?;
        if !chunk.chars().eq(chars[offset..end].iter().copied()) {
            return Ok(false);
        }
        offset = end;
    }
    Ok(true)
}

async fn utf16_readback_matches(
    text: &TextProxy<'_>,
    requested_utf16: &[u16],
    deadline: Instant,
) -> Result<bool, ReadbackError> {
    let mut offset = 0;
    while offset < requested_utf16.len() {
        let end = utf16_chunk_end(requested_utf16, offset, EDITABLE_READ_CHUNK_UNITS);
        let chunk = readback_within(deadline, text.get_text(offset as i32, end as i32)).await?;
        if !chunk
            .encode_utf16()
            .eq(requested_utf16[offset..end].iter().copied())
        {
            return Ok(false);
        }
        offset = end;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_control_chars_with_multiline_policy() {
        assert!(validate_editable_input("hello world", false).is_ok());
        assert!(validate_editable_input("line1\nline2", false).is_err());
        assert!(validate_editable_input("line1\nline2\tcol", true).is_ok());
        assert!(validate_editable_input("bad\x00char", true).is_err());
    }

    #[test]
    fn avoids_splitting_utf16_surrogate_pairs_on_chunk_boundary() {
        let sample = "a😀b"; // 'a' (1), '😀' (2 units: high+low surrogate), 'b' (1)
        let units: Vec<u16> = sample.encode_utf16().collect();
        assert_eq!(units.len(), 4);
        // Requesting chunk of 2 stops at index 1 because units[1] is high surrogate.
        assert_eq!(utf16_chunk_end(&units, 0, 2), 1);
        assert_eq!(utf16_chunk_end(&units, 1, 2), 3);
        assert_eq!(utf16_chunk_end(&units, 3, 2), 4);
    }

    #[test]
    fn detects_password_roles() {
        assert!(is_password_role(Role::PasswordText, ""));
        assert!(is_password_role(Role::Invalid, "Password Text"));
        assert!(!is_password_role(Role::Text, "text"));
    }
}
