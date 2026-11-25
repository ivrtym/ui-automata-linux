pub use crate::input::{ClickType, ScrollAxis};
use crate::{Result, input};

/// Move the mouse cursor to `(x, y)`.
pub fn move_cursor(x: i32, y: i32) -> Result<()> {
    input::mouse_move(x, y)
}

/// Return current mouse cursor coordinates `(x, y)`.
pub fn get_cursor_pos() -> Result<(i32, i32)> {
    input::get_cursor_pos()
}

/// Scroll the mouse wheel at the current cursor position.
pub fn scroll_wheel(delta: i32, axis: ScrollAxis) -> Result<()> {
    input::scroll_axis_ticks(delta, axis)
}
