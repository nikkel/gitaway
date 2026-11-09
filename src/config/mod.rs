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
//! - [`value`] holds the validated value types profiles use (keys, fingerprints).
//! - [`resolve`] picks the profile for a command.
//! - [`store`] loads and saves the files, safely.
//!
//! Secrets never appear in these files. They live in the OS keychain.
//!
//! # Extending
//!
//! A new profile field goes in the matching struct in [`profile`], with a
//! test showing it parses and that invalid values are rejected. Document it in
//! `docs/configuration.md` in the same change.

pub mod name;
pub mod paths;
pub mod profile;
pub mod resolve;
pub mod store;
pub mod value;
