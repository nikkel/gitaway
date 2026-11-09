//! Device keys: the WireGuard private key each profile uses for its tunnel.
//!
//! Every profile has its own key, created by `gitaway profile add` and
//! replaced only by `gitaway key rotate`. The server only ever learns the
//! public half, printed by `gitaway peer`.
//!
//! - [`device_key`] is the key itself: generated from the OS's secure random
//!   source, wiped from memory when dropped, never printed.
//! - [`store`] keeps keys in the OS keychain, or in private files where
//!   there's no keychain.
//!
//! Keys are never written to profile files, logs, command lines or the
//! environment.

pub mod device_key;
pub mod store;
