//! `gitaway use <profile>`: pin the current repository to a profile.
//!
//! Writes `gitaway.profile` into the repository's own git config
//! (`.git/config`), which is the most common way a profile gets chosen.

use std::ffi::OsString;

use crate::config::name::ProfileName;
use crate::config::store::StoreError;
use crate::error::Error;
use crate::git::repo::Repo;
use crate::git::runner::Git;

use super::args::UseArgs;

/// Runs `gitaway use`. `global_options` are git options typed before the
/// command, such as `-C dir`, so `gitaway -C app use home` pins `app`.
pub fn run(args: UseArgs, global_options: &[OsString]) -> Result<i32, Error> {
    let name: ProfileName = args
        .profile
        .parse()
        .map_err(|e| Error::msg(format!("{e}")))?;
    let store = super::open_store()?;
    if !store.profile_exists(&name) {
        return Err(StoreError::NotFound(name).into());
    }
    Repo::new(Git::new(global_options)).set_profile(name.as_str())?;
    println!("This repository now uses profile `{name}`.");
    Ok(0)
}
