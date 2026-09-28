//! Core of Tx3 Fluent: the contracts shared by every Fluent component.
//!
//! - [`config`] — the server configuration model and its loader.
//! - [`error`] — [`FluentError`], the error every operation reports.
//! - [`envelope`] — [`PreparedTransaction`], the result of preparing a
//!   transaction.
//! - [`registration`] — registration bundles, loaded into a [`Catalog`].
//! - [`catalog`] — the MCP tools a [`Catalog`] offers, as [`ToolDescriptor`]s.
//! - [`engine`] — [`Engine`], which prepares a registration's transactions
//!   through a TRP resolver.
//! - [`summary`] — the reviewable summary decoded from a transaction's CBOR.

pub mod catalog;
pub mod config;
pub mod engine;
pub mod envelope;
pub mod error;
pub mod registration;
pub mod summary;

pub use catalog::ToolDescriptor;
pub use config::Config;
pub use engine::{Engine, PrepareRequest};
pub use envelope::PreparedTransaction;
pub use error::{ErrorCode, FluentError};
pub use registration::{Catalog, Registration};
