//! What happens to a network command (clone, fetch, pull, push …).
//!
//! [`plan`] decides which git command to run, or refuses with a reason:
//!
//! ```text
//! no profiles configured ─────────────────────────► git as typed
//! find the remote URL ─► pick the profile (config::resolve)
//!   no profile, unknown remote, or the profile's
//!   [hosts] doesn't handle the remote's host ──────► git as typed
//!   home check passes ─────────────────────────────► git as typed
//!                                                     (clone also pins the profile)
//!   home check fails ──────────────────────────────► error with the reason
//!                                                     (milestone 2: tunnel)
//! ```
//!
//! # Extending
//!
//! Milestone 2 replaces the "home check fails" error with opening a tunnel
//! and returning a git command pointed at it.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::config::name::ProfileName;
use crate::config::profile::Profile;
use crate::config::resolve::{self, Inputs};
use crate::error::Error;
use crate::git::classify::{Invocation, RemoteArg};
use crate::git::remote_url;
use crate::git::repo::{PROFILE_KEY, Repo};
use crate::git::runner::{Git, RunError};
use crate::home_check::{self, AwayReason, Verdict};

/// The environment variable that names a profile.
pub const PROFILE_ENV: &str = "GITAWAY_PROFILE";

/// Decides the git arguments to run for a network command, or why not.
///
/// `invocation` is the classified command line, and `profile_flag` the value
/// of `--profile`, if given.
pub fn plan(
    invocation: &Invocation<'_>,
    profile_flag: Option<&ProfileName>,
) -> Result<Vec<OsString>, Error> {
    let as_typed = as_typed(invocation);
    let env_profile = env_profile()?;

    let store = super::open_store()?;
    let profiles = store.load_profiles()?;
    if profiles.is_empty() && profile_flag.is_none() && env_profile.is_none() {
        return Ok(as_typed);
    }
    let global = store.load_global()?;

    let repo = Repo::new(Git::new(invocation.global_options));
    let (git_config_profile, url) = match query_repo(&repo, invocation) {
        Ok(facts) => facts,
        // A lookup git itself refused (a bad `-C dir`, a broken config file)
        // fails the real command in exactly the same way, before any network
        // access. Let git report it, with its own message and exit code.
        Err(RunError::Failed { .. }) => return Ok(as_typed),
        Err(e) => return Err(e.into()),
    };
    let git_config_profile = git_config_profile
        .map(|name| name.parse::<ProfileName>())
        .transpose()
        .map_err(|e| Error::msg(format!("git config {PROFILE_KEY}: {e}")))?;
    let remote = url.as_deref().and_then(remote_url::parse);

    let inputs = Inputs {
        flag: profile_flag,
        env: env_profile.as_ref(),
        git_config: git_config_profile.as_ref(),
        remote: remote.as_ref(),
        default: global.default_profile.as_ref(),
    };
    // Home checks run while resolving are kept, so none runs twice.
    let mut verdicts: BTreeMap<ProfileName, Verdict> = BTreeMap::new();
    let resolved = resolve::resolve(inputs, &profiles, |name, profile| {
        let verdict = verdict_if_checkable(profile);
        let home = verdict == Some(Verdict::Home);
        if let Some(verdict) = verdict {
            verdicts.insert(name.clone(), verdict);
        }
        home
    })?;

    let Some(resolved) = resolved else {
        return Ok(as_typed);
    };
    let Some(remote) = remote else {
        return Ok(as_typed);
    };
    if !resolved.profile.hosts.handles(&remote) {
        return Ok(as_typed);
    }

    let verdict = match verdicts.remove(resolved.name) {
        Some(verdict) => verdict,
        None => home_verdict(resolved.name, resolved.profile)?,
    };
    match verdict {
        Verdict::Home => Ok(pin_clone(invocation, resolved.name).unwrap_or(as_typed)),
        Verdict::Away(reason) => Err(away_error(resolved.name, resolved.profile, &reason)),
    }
}

/// The command exactly as typed (after gitaway's own `--profile`).
fn as_typed(invocation: &Invocation<'_>) -> Vec<OsString> {
    let mut args = invocation.global_options.to_vec();
    args.extend(invocation.subcommand.map(OsString::from));
    args.extend(invocation.args.iter().cloned());
    args
}

/// `GITAWAY_PROFILE`, if set and not empty.
fn env_profile() -> Result<Option<ProfileName>, Error> {
    match std::env::var(PROFILE_ENV) {
        Ok(value) if !value.is_empty() => value
            .parse()
            .map(Some)
            .map_err(|e| Error::msg(format!("{PROFILE_ENV}: {e}"))),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(Error::msg(format!("{PROFILE_ENV} isn't valid UTF-8")))
        }
    }
}

/// What the repository says: `gitaway.profile`, and the URL the command
/// will talk to.
fn query_repo(
    repo: &Repo,
    invocation: &Invocation<'_>,
) -> Result<(Option<String>, Option<String>), RunError> {
    Ok((repo.profile()?, remote_url_of(repo, invocation)?))
}

/// The URL the command will talk to: a remote or URL named in the
/// arguments, else git's default remote for this command. `None` for a
/// clone of a local path.
fn remote_url_of(repo: &Repo, invocation: &Invocation<'_>) -> Result<Option<String>, RunError> {
    let names = repo.remote_names()?;
    let direction = invocation.direction();
    let url = match invocation.remote_argument(|arg| names.iter().any(|name| name == arg)) {
        Some(RemoteArg::Url(url)) => Some(repo.expand_url(url)?),
        Some(RemoteArg::Name(name)) => repo.remote_url(name, direction)?,
        None if invocation.subcommand == Some("clone") => None,
        None => {
            let remote = repo.default_remote(direction)?;
            repo.remote_url(&remote, direction)?
        }
    };
    Ok(url)
}

/// Runs the home check for a profile that will be used.
fn home_verdict(name: &ProfileName, profile: &Profile) -> Result<Verdict, Error> {
    let Some(home) = &profile.home else {
        return Err(Error::msg(format!(
            "profile `{name}` has no [home] section, so gitaway can't tell whether you're home.\n\
             Add one with `gitaway profile set {name} home.ssh <host:port>` and \
             `gitaway profile set {name} home.host_key '<key>'`"
        )));
    };
    Ok(home_check::check(home)?)
}

/// The home check for choosing between several matching profiles. Profiles
/// that can't be checked (no `[home]`, or the check couldn't run) count as
/// not home.
fn verdict_if_checkable(profile: &Profile) -> Option<Verdict> {
    profile
        .home
        .as_ref()
        .and_then(|home| home_check::check(home).ok())
}

/// For `clone`, adds `--config gitaway.profile=<name>` so the new repository
/// remembers its profile. `None` for other commands.
fn pin_clone(invocation: &Invocation<'_>, name: &ProfileName) -> Option<Vec<OsString>> {
    if invocation.subcommand != Some("clone") {
        return None;
    }
    let mut args = invocation.global_options.to_vec();
    args.push("clone".into());
    args.push("--config".into());
    args.push(format!("{PROFILE_KEY}={name}").into());
    args.extend(invocation.args.iter().cloned());
    Some(args)
}

fn away_error(name: &ProfileName, profile: &Profile, reason: &AwayReason) -> Error {
    let server = profile
        .home
        .as_ref()
        .map_or_else(|| "the home server".to_owned(), |home| home.ssh.to_string());
    let message = match reason {
        AwayReason::WrongKey { fingerprint } => format!(
            "{server} answered with a different host key ({fingerprint}) than profile `{name}` expects.\n\
             If you're home, the server's key may have changed: check it on the server, then update it with\n\
             `gitaway profile set {name} home.host_key '<new key>'`"
        ),
        other => format!(
            "not on the home network for profile `{name}` ({server}: {other}).\n\
             Reaching it from elsewhere needs the tunnel, which isn't built yet"
        ),
    };
    Error::msg(message)
}
