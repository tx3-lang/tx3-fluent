//! Core of Tx3 Fluent: the contracts shared by every Fluent component.
//!
//! - [`config`] — the server configuration model and its loader.
//! - [`error`] — [`FluentError`], the error every operation reports.
//! - [`envelope`] — [`PreparedTransaction`], the result of preparing a
//!   transaction.
//! - [`registration`] — registration bundles, loaded into a [`Catalog`].
//! - [`catalog`] — the MCP tools a [`Catalog`] offers, as [`ToolDescriptor`]s.

pub mod catalog;
pub mod config;
pub mod envelope;
pub mod error;
pub mod registration;

pub use catalog::ToolDescriptor;
pub use config::Config;
pub use envelope::PreparedTransaction;
pub use error::{ErrorCode, FluentError};
pub use registration::{Catalog, Registration};
