//! Process hardening: making gitaway's memory harder for other programs to
//! read while it holds a secret.
//!
//! gitaway keeps the device's private key (and, away from home, the proxy's
//! token) in its own memory. [`harden_process`] runs first thing in
//! [`crate::cli::main`], before any secret is loaded.
//!
//! On Linux it marks the process not dumpable (`PR_SET_DUMPABLE` = 0):
//!
//! - no core dump is written if gitaway crashes, so the key never lands in a
//!   file on disk;
//! - other processes running as the same user can no longer attach a
//!   debugger (`ptrace`) or read `/proc/<pid>/mem` and `/proc/<pid>/environ`.
//!   Only root can.
//!
//! The setting applies to gitaway only. A program gitaway starts (git, ssh)
//! is dumpable again, because the kernel resets the flag when a process
//! runs a new program.
//!
//! On other systems it does nothing yet.
//!
//! # Extending
//!
//! Another platform's equivalent (such as macOS's `PT_DENY_ATTACH`) is a new
//! `#[cfg]` branch in [`harden_process`]. Keep each step one the process can
//! apply to itself without privileges.

use std::io;

/// Applies every hardening step this platform supports. Call it once, at
/// start-up, before loading a secret.
///
/// # Errors
///
/// If the system refuses a step, which a sandbox that blocks `prctl` could
/// do. gitaway warns and carries on: this is a layer on top of the key
/// store's own protection, and refusing would also stop plain git commands.
pub fn harden_process() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{DumpableBehavior, set_dumpable_behavior};
        set_dumpable_behavior(DumpableBehavior::NotDumpable)?;
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_process_is_not_dumpable_afterwards() {
        use rustix::process::{DumpableBehavior, dumpable_behavior};
        harden_process().unwrap();
        assert_eq!(dumpable_behavior().unwrap(), DumpableBehavior::NotDumpable);
    }
}
