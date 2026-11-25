//! Structured error classification for the unified `AutomationService`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum ServiceError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("window not found: {0}")]
    WindowNotFound(String),
    #[error("element not found: {0}")]
    ElementNotFound(String),
    #[error("capability unsupported: {0}")]
    CapabilityUnsupported(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("operation timed out: {0}")]
    Timeout(String),
    #[error("operation cancelled: {0}")]
    Cancelled(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("platform error: {0}")]
    Platform(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ServiceError {
    pub fn from_anyhow(err: anyhow::Error) -> Self {
        let msg = err.to_string();
        Self::classify_message(msg)
    }

    pub fn classify_message(msg: String) -> Self {
        let low = msg.to_ascii_lowercase();
        if low.contains("accessdenied")
            || low.contains("permission denied")
            || low.contains("disabled") && low.contains("file writes")
        {
            Self::PermissionDenied(msg)
        } else if low.contains("cancel") {
            Self::Cancelled(msg)
        } else if low.contains("timed out") || low.contains("timeout") {
            Self::Timeout(msg)
        } else if low.contains("already running") {
            Self::Conflict(msg)
        } else if low.contains("not found") && low.contains("window") {
            Self::WindowNotFound(msg)
        } else if low.contains("not found") && low.contains("element") {
            Self::ElementNotFound(msg)
        } else if low.contains("unsupported") || low.contains("not implemented") {
            Self::CapabilityUnsupported(msg)
        } else if low.contains("requires ")
            || low.contains("invalid ")
            || low.contains("must specify")
            || low.contains("out of safe")
        {
            Self::InvalidInput(msg)
        } else {
            Self::Platform(msg)
        }
    }
}

impl From<anyhow::Error> for ServiceError {
    fn from(value: anyhow::Error) -> Self {
        Self::from_anyhow(value)
    }
}

impl From<automata_linux::UiError> for ServiceError {
    fn from(value: automata_linux::UiError) -> Self {
        Self::classify_message(value.to_string())
    }
}

impl From<ui_automata::AutomataError> for ServiceError {
    fn from(value: ui_automata::AutomataError) -> Self {
        Self::classify_message(value.to_string())
    }
}

impl From<std::io::Error> for ServiceError {
    fn from(value: std::io::Error) -> Self {
        match value.kind() {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied(value.to_string()),
            std::io::ErrorKind::TimedOut => Self::Timeout(value.to_string()),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput => {
                Self::InvalidInput(value.to_string())
            }
            _ => Self::Platform(value.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_errors() {
        assert!(matches!(
            ServiceError::classify_message("matching window not found".into()),
            ServiceError::WindowNotFound(_)
        ));
        assert!(matches!(
            ServiceError::classify_message("timed out after 15s waiting for launch".into()),
            ServiceError::Timeout(_)
        ));
        assert!(matches!(
            ServiceError::classify_message("another workflow is already running".into()),
            ServiceError::Conflict(_)
        ));
    }
}
