//! Tx3 Fluent development and evidence tooling, run as `cargo xtask`.
//!
//! Nothing here is part of the `fluent` binary, the container image or a
//! release: the runtime stays free of commands that only produce evidence.
//!
//! - [`verify`] — `cargo xtask verify`: an independent check of a
//!   transaction's CBOR against the outputs, network and signers a person
//!   expects, decoded by [`decode`] rather than the runtime's summary.
//! - [`transcript`] — `cargo xtask transcript`: a Markdown transcript of the
//!   tools a server listed and called, rendered offline from its JSON logs.

pub mod decode;
pub mod transcript;
pub mod verify;
