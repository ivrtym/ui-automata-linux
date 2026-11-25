//! Non-stealing background UI input via AT-SPI2 (`Action`, `EditableText`)
//! without warping the mouse cursor or stealing desktop keyboard focus.

use atspi::State;

use crate::a11y::editable::EditableSetOutcome;
use crate::a11y::element::LinuxElement;
use crate::UiError;

/// Activates an accessible element in the background using its AT-SPI2
/// `Action` interface (`invoke`), avoiding pointer warps or focus changes.
pub fn background_invoke_element(el: &LinuxElement) -> Result<(), UiError> {
    el.invoke()
}

/// Writes text or numeric contents directly into an accessible element via
/// AT-SPI2 (`EditableText` / `Value`) with readback verification and password
/// protection, without moving focus or injecting global keystrokes.
pub fn background_set_element_value(
    el: &LinuxElement,
    value: &str,
) -> Result<EditableSetOutcome, UiError> {
    el.set_value_verified(value)
}

/// Maps unambiguous single keys (`Return`/`Enter`, `space`) on appropriate AT-SPI2
/// controls to background AT-SPI actions without stealing global focus.
pub fn map_background_key_to_action<'a>(
    role: &str,
    states: &[&str],
    key: &str,
    available_actions: &[&'a str],
) -> Result<&'a str, UiError> {
    let trimmed = key.trim().to_ascii_lowercase();
    let is_enter = matches!(trimmed.as_str(), "enter" | "return" | "{enter}" | "kp_enter");
    let is_space = matches!(trimmed.as_str(), "space" | " " | "{space}");
    if !is_enter && !is_space {
        return Err(UiError::Platform(format!(
            "background key {key:?} requires global focus; only Enter/Return/space map safely via AT-SPI Action"
        )));
    }

    let role_l = role.trim().to_ascii_lowercase();
    let has_state = |needle: &str| states.iter().any(|s| s.eq_ignore_ascii_case(needle));

    let candidates: &[&str] = match role_l.as_str() {
        "button" | "push button" | "menu item" | "menuitem" => &["click", "press", "activate"],
        "link" | "hyperlink" if is_enter => &["jump", "click", "press", "activate"],
        "checkbox" | "check box" | "radio" | "radio button" | "toggle button" if is_space => {
            &["toggle", "click", "press", "check", "uncheck", "activate"]
        }
        "edit" | "text" | "entry" if is_enter => {
            if !has_state("single-line") || has_state("multi-line") {
                return Err(UiError::Platform(format!(
                    "refusing background Enter on non-single-line {role_l} to avoid unintended newlines"
                )));
            }
            &["activate"]
        }
        _ => {
            return Err(UiError::Platform(format!(
                "background key {key:?} has no unambiguous AT-SPI action for role '{role_l}'"
            )));
        }
    };

    candidates
        .iter()
        .find_map(|cand| {
            available_actions
                .iter()
                .find(|act| act.trim().eq_ignore_ascii_case(cand))
                .copied()
        })
        .ok_or_else(|| {
            UiError::Platform(format!(
                "element role '{role_l}' exposes none of ({}) for background key {key:?}",
                candidates.join(", ")
            ))
        })
}

/// Checks if a `LinuxElement`'s live AT-SPI states indicate single-line input.
pub fn element_is_single_line(el: &LinuxElement) -> bool {
    el.states()
        .map(|s| s.contains(State::SingleLine) && !s.contains(State::MultiLine))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_safe_background_keys_and_rejects_multiline_enter() {
        assert_eq!(
            map_background_key_to_action("button", &[], "{ENTER}", &["press"]).unwrap(),
            "press"
        );
        assert_eq!(
            map_background_key_to_action("checkbox", &[], "space", &["toggle"]).unwrap(),
            "toggle"
        );
        assert_eq!(
            map_background_key_to_action("edit", &["single-line"], "Return", &["activate"])
                .unwrap(),
            "activate"
        );
        assert!(
            map_background_key_to_action("edit", &["multi-line"], "Return", &["activate"])
                .is_err()
        );
        assert!(map_background_key_to_action("button", &[], "ctrl+c", &["press"]).is_err());
    }
}
