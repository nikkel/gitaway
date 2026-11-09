//! Everything gitaway knows about git.
//!
//! gitaway never reimplements git. This module only inspects the arguments a
//! user passes, so gitaway can decide whether a command needs the network,
//! and parses remote URLs so they can be matched against profiles.
//!
//! - [`classify`] finds the git subcommand past git's global options and says
//!   whether it talks to a remote.
//! - [`remote_url`] extracts the host and port from every URL form git accepts.
//!
//! Both are pure: no I/O, no process spawning. That keeps them fast and fully
//! unit-testable. Code that runs git or reads its config belongs in a separate
//! module.

pub mod classify;
pub mod remote_url;
