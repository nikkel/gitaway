//! The error type for gitaway's commands, and how errors become exit codes.
//!
//! Each module has its own precise error type. This one wraps them all, so
//! the CLI can return any of them with `?` and print one clear message.
//!
//! Exit codes: gitaway's own errors exit with [`EXIT_ERROR`]. When git runs,
//! gitaway exits with git's code instead, unchanged. Usage errors from
//! argument parsing exit with 2, as clap does.
//!
//! # Extending
//!
//! A new module error gets a `#[from]` variant here, so `?` converts it.

use crate::config::paths::NoConfigDir;
use crate::config::resolve::ResolveError;
use crate::config::store::StoreError;
use crate::git::runner::RunError;
use crate::home_check::CheckError;
use crate::keys::store::KeyError;
use crate::transport::TransportError;

/// The exit code for gitaway's own errors.
pub const EXIT_ERROR: i32 = 1;

/// Anything that can stop a gitaway command.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No configuration directory could be found.
    #[error(transparent)]
    ConfigDir(#[from] NoConfigDir),
    /// Reading or writing configuration files failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// No profile could be chosen safely.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// git couldn't be run, or a git query failed.
    #[error(transparent)]
    Git(#[from] RunError),
    /// The home check couldn't run.
    #[error(transparent)]
    HomeCheck(#[from] CheckError),
    /// A device key couldn't be read, saved or deleted.
    #[error(transparent)]
    Key(#[from] KeyError),
    /// The tunnel couldn't be opened.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// Anything else, with a message for the user.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Shorthand for [`Error::Other`].
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }
}
