//! Linux AT-SPI2 accessibility client, concurrent tree snapshot engine,
//! editable text verification, and payload compaction/spill helpers.

pub mod client;
pub mod editable;
pub mod encoding;
pub mod element;
pub mod tree;

pub use client::*;
pub use editable::*;
pub use element::*;
pub use encoding::*;
pub use tree::*;
