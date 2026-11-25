//! RAII key/button release guards ensuring synthetic modifiers and pointer
//! buttons never remain stuck down if an input step returns early or panics.

/// Tracks held input codes and releases them in reverse (LIFO) order on drop
/// unless explicitly released beforehand via [`InputReleaseGuard::release_all`].
pub struct InputReleaseGuard<C: Copy, F: FnMut(C)> {
    held: Vec<C>,
    release: F,
}

impl<C: Copy, F: FnMut(C)> InputReleaseGuard<C, F> {
    pub fn new(release: F) -> Self {
        Self {
            held: Vec::new(),
            release,
        }
    }

    /// Records `code` as currently pressed so Drop will release it.
    pub fn push(&mut self, code: C) {
        self.held.push(code);
    }

    /// Explicitly releases all held codes in reverse order and clears tracking.
    pub fn release_all(&mut self) {
        while let Some(code) = self.held.pop() {
            (self.release)(code);
        }
    }

    pub fn held_len(&self) -> usize {
        self.held.len()
    }
}

impl<C: Copy, F: FnMut(C)> Drop for InputReleaseGuard<C, F> {
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn releases_in_reverse_order_on_drop() {
        let log = Rc::new(RefCell::new(Vec::new()));
        {
            let log_ref = Rc::clone(&log);
            let mut guard = InputReleaseGuard::new(move |code: u32| {
                log_ref.borrow_mut().push(code);
            });
            guard.push(29);
            guard.push(42);
            guard.push(30);
            assert_eq!(guard.held_len(), 3);
        }
        assert_eq!(*log.borrow(), vec![30, 42, 29]);
    }

    #[test]
    fn explicit_release_does_not_double_release_on_drop() {
        let log = Rc::new(RefCell::new(Vec::new()));
        {
            let log_ref = Rc::clone(&log);
            let mut guard = InputReleaseGuard::new(move |code: u32| {
                log_ref.borrow_mut().push(code);
            });
            guard.push(1);
            guard.release_all();
            assert_eq!(guard.held_len(), 0);
        }
        assert_eq!(*log.borrow(), vec![1]);
    }
}
