//! Tx3 Fluent server: the transports that expose the Fluent core.
//!
//! - [`mcp`] — [`FluentHandler`](mcp::FluentHandler), the MCP server over a
//!   loaded catalog, independent of the transport that carries it.
//! - [`http`] — the Streamable HTTP transport and its caller
//!   [authentication](http::auth).
//! - [`store`] — the hosted store of users and their selections, and the
//!   per-user tool scopes it backs.
//! - [`site`] — the companion site where users sign in and choose their
//!   registrations.

pub mod http;
pub mod mcp;
pub mod site;
pub mod store;
