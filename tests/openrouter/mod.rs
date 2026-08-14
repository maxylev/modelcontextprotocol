//! Shared modules for the OpenRouter end-to-end acceptance suite.
//!
//! The suite is gated: the online test lives in `tests/openrouter_e2e.rs`
//! behind `#[ignore]` and requires `OPENROUTER_API_KEY`. The modules here
//! also contain offline unit tests (schema normalizer/validator) that run in
//! ordinary `cargo test`.

pub mod cases;
pub mod harness;
pub mod report;
pub mod schema;
