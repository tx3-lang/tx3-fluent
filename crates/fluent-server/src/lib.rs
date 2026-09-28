//! Tx3 Fluent server: the transports that expose the Fluent core.
//!
//! - [`mcp`] — [`FluentHandler`](mcp::FluentHandler), the MCP server over a
//!   loaded catalog, independent of the transport that carries it.

pub mod mcp;
