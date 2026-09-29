//! What `fluent serve` may log.
//!
//! rmcp logs every JSON-RPC message it receives, tool arguments included, at
//! `debug` and `trace`. [`no_message_bodies`] drops those events whatever
//! `RUST_LOG` enables, so raising the log level never exposes an argument
//! value. It drops the [transcript](crate::transcript) events too: a
//! transcript records redacted results, which do not belong in the logs.

use tracing::{Level, Metadata};
use tracing_subscriber::filter::{FilterFn, filter_fn};

/// Drops rmcp's events more verbose than `info`, and the transcript events.
pub fn no_message_bodies() -> FilterFn<fn(&Metadata<'_>) -> bool> {
    filter_fn(allowed as fn(&Metadata<'_>) -> bool)
}

fn allowed(metadata: &Metadata<'_>) -> bool {
    let rmcp = metadata.target() == "rmcp" || metadata.target().starts_with("rmcp::");
    !(rmcp && *metadata.level() > Level::INFO) && metadata.target() != crate::transcript::TARGET
}
