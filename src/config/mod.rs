//! gitaway's own configuration: where it lives, what a profile contains, and
//! which profile applies to a command.
//!
//! A *profile* is one place you connect to, such as home or the office. Each
//! is a TOML file in `~/.config/gitaway/profiles/`, and `config.toml` next to
//! that directory holds settings that aren't per profile.
//!
//! - [`paths`] works out where the files live.
//! - [`name`] defines which profile names are allowed.
//! - [`profile`] defines the profile format and validates it.
//! - [`value`] holds the validated value types profiles use (host keys, WireGuard keys, addresses).
//! - [`resolve`] picks the profile for a command.
//! - [`lock`] stops two commands using one profile's tunnel at once.
//! - [`store`] loads and saves the files, safely, using `private_fs` for atomic, private writes.
//!
//! Secrets never appear in these files. They live in the OS keychain.
//!
//! # Extending
//!
//! A new profile field goes in the matching struct in [`profile`], with a
//! test showing it parses and that invalid values are rejected. Document it in
//! `docs/configuration.md` in the same change.

pub mod lock;
pub mod name;
pub mod paths;
pub(crate) mod private_fs;
pub mod profile;
pub mod resolve;
pub mod store;
pub mod value;
