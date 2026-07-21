//! OxidEQ — bit-perfect parametric EQ pipeline.
//!
//! Modules are added task by task; `main.rs` stays a thin dispatcher.

pub mod cli;
pub mod devices;
pub mod dsp;
pub mod engine;
pub mod preset;
pub mod routing;
