//! Library half of `lemmate-cli`: the REST client, the MCP server and the browser sign-in, so integration tests can
//! drive them in-process. See `main.rs` for the binary.

pub mod browser;
pub mod mcp;
pub mod remote;
