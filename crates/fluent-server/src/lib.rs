//! Tx3 Fluent server: the transports that expose the Fluent core.
//!
//! - [`mcp`] — [`FluentHandler`](mcp::FluentHandler), the MCP server over a
//!   loaded catalog, independent of the transport that carries it.
//! - [`http`] — the Streamable HTTP transport and its caller
//!   [authentication](http::auth).

pub mod http;
pub mod mcp;
