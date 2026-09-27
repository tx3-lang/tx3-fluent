//! Core of Tx3 Fluent: the contracts shared by every Fluent component.
//!
//! - [`config`] — the server configuration model and its loader.
//! - [`error`] — [`FluentError`], the error every operation reports.
//! - [`envelope`] — [`PreparedTransaction`], the result of preparing a
//!   transaction.

pub mod config;
pub mod envelope;
pub mod error;

pub use config::Config;
pub use envelope::PreparedTransaction;
pub use error::{ErrorCode, FluentError};
