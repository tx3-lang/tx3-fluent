//! Tx3 Fluent server: the transports that expose the Fluent core.
//!
//! - [`mcp`] — [`FluentHandler`](mcp::FluentHandler), the MCP server over a
//!   loaded catalog, independent of the transport that carries it.
//! - [`http`] — the Streamable HTTP transport and its caller
//!   [authentication](http::auth).
//! - [`store`] — the hosted store of users and their selections, and the
//!   per-user tool scopes it backs.
//! - [`limits`] — the daily quota, concurrency gate and global cutoff on
//!   transaction tools.
//! - [`metrics`] — the service measurements served at `GET /metrics`.
//! - [`logging`] — the filter that keeps message bodies out of the logs.
//! - [`transcript`] — the Markdown transcript of the tools listed and called
//!   that `fluent demo record` writes.
//! - [`site`] — the companion site where users sign in and choose their
//!   registrations.

pub mod http;
pub mod limits;
pub mod logging;
pub mod mcp;
pub mod metrics;
pub mod site;
pub mod store;
pub mod transcript;
