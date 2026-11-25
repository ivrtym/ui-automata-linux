use super::{LinuxElement, Selector, UiError};

/// Scoped element finder: searches within a root element for a selector match.
pub struct Locator {
    root: LinuxElement,
    selector: Selector,
}

impl Locator {
    pub(crate) fn new(root: LinuxElement, selector: Selector) -> Self {
        Self { root, selector }
    }

    /// Search the subtree of `root` for the first element matching the selector.
    /// Returns an error if no match is found.
    pub fn find(&self) -> Result<LinuxElement, UiError> {
        find_in(&self.root, &self.selector)
    }
}

fn matches(el: &LinuxElement, selector: &Selector) -> bool {
    match selector {
        Selector::Role { role, name } => {
            if el.role() != *role {
                return false;
            }
            match name {
                None => true,
                Some(n) => el.name_or_empty() == *n,
            }
        }
    }
}

fn find_in(el: &LinuxElement, selector: &Selector) -> Result<LinuxElement, UiError> {
    let children = el.children_elems()?;
    for child in children {
        if matches(&child, selector) {
            return Ok(child);
        }
        if let Ok(found) = find_in(&child, selector) {
            return Ok(found);
        }
    }
    Err(UiError::Internal(format!(
        "Element not found: {selector:?}"
    )))
}
