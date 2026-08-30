//! Library half of `notes-cli`: the REST client and the MCP server, so integration tests can
//! drive them in-process. See `main.rs` for the binary.

pub mod mcp;
pub mod remote;
