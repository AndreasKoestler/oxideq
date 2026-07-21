//! OxidEQ — bit-perfect parametric EQ pipeline.
//!
//! `main.rs` stays a thin dispatcher; the logic lives in these modules.

pub mod cli;
pub mod devices;
pub mod dsp;
pub mod engine;
pub mod preset;
pub mod resample;
