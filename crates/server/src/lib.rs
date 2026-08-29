//! Library half of notes-server so integration tests can drive the router in-process.
//! See `main.rs` for the binary entry point.

pub mod app;

pub use app::{AppState, ServerOptions, build_state, router};
