//! Helper utilities for the CLI application.

pub mod session;
pub mod formatting;
pub mod input;
pub mod context;  // 🆕 Context refinement helpers

pub use session::*;
pub use formatting::*;
pub use input::*;
pub use context::*;
