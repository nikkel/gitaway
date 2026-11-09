//! # gitaway
//!
//! Reach a self-hosted git server from anywhere. At home, gitaway runs git
//! directly. Away from home, it opens a VPN tunnel that only the gitaway
//! process uses: no system-wide VPN, no virtual network adapter, no root.
//!
//! gitaway wraps the `git` binary rather than reimplementing it. Commands it
//! doesn't own are passed to git unchanged, and git's exit code is returned
//! unchanged.
//!
//! This crate is a library with two thin binaries (`gitaway` and `git-away`,
//! so `git away push` works). Nearly all the logic lives here, so it can be
//! tested and documented directly.
//!
//! ## Stability
//!
//! The stability promise covers the command-line interface and the profile
//! file format. The library API is an internal detail of the binaries and may
//! change in any release.
//!
//! See `docs/` in the repository for the architecture and contributor guides.

pub mod address;
pub mod config;
pub mod git;
