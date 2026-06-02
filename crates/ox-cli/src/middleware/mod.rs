//! Middleware module for processing events and applying transformations.
//!
//! This module provides a pluggable middleware chain for handling:
//! - Implicit feedback detection
//! - User interjection handling

pub mod feedback;
pub mod interjection;
