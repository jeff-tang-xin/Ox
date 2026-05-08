//! Middleware module for processing events and applying transformations.
//!
//! This module provides a pluggable middleware chain for handling:
//! - Context compression
//! - Implicit feedback detection
//! - User interjection handling

pub mod compression;
pub mod feedback;
pub mod interjection;
