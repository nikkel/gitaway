//! Everything gitaway knows about git.
//!
//! gitaway never reimplements git. It inspects the arguments a user passes,
//! so it can decide whether a command needs the network, parses remote URLs
//! so they can be matched against profiles, and runs the user's own git.
//!
//! - [`classify`] finds the git subcommand past git's global options and says
//!   whether it talks to a remote.
//! - [`remote_url`] extracts the host and port from every URL form git accepts.
//!
//! - [`runner`] runs the user's installed git.
//! - [`repo`] reads and writes git config through git.
//!
//! `classify` and `remote_url` are pure: no I/O, no processes, so they're fast
//! and fully unit-testable. Only `runner` starts processes, and `repo` goes
//! through it.

pub mod classify;
pub mod remote_url;
pub mod repo;
pub mod runner;
