/// Error type for Linux UI automation operations.
#[derive(Debug)]
pub enum UiError {
    /// A semantic/logic error (e.g. element not found, unexpected state).
    Internal(String),
    /// A platform API error (AT-SPI2 / D-Bus / OS input).
    Platform(String),
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal(s) | Self::Platform(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for UiError {}

impl From<UiError> for ui_automata::AutomataError {
    fn from(e: UiError) -> Self {
        match e {
            UiError::Internal(s) => ui_automata::AutomataError::Internal(s),
            UiError::Platform(s) => ui_automata::AutomataError::Platform(s),
        }
    }
}

impl From<anyhow::Error> for UiError {
    fn from(e: anyhow::Error) -> Self {
        UiError::Platform(e.to_string())
    }
}
