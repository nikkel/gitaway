//! The command line: decide what a command is, then hand it to the right
//! place.
//!
//! ```text
//! gitaway [--profile <name>] [git global options] <command> [args]
//!
//!   no command, -h, --help ─────────► gitaway's help
//!   --version ──────────────────────► gitaway's version
//!   setup, profile, use, peer, key, ►
//!   check, tunnel                    gitaway's own commands (clap)
//!   network command (push, fetch …) ► network::plan, then git directly,
//!                                     or through the tunnel (away)
//!   anything else ──────────────────► git, exactly as typed
//! ```
//!
//! clap only ever sees gitaway's own commands. Everything else is classified
//! with [`crate::git::classify`], because clap would reject git's options
//! (`-C dir`, `-c key=value`).
//!
//! `--profile` must come first, before any git options.
//!
//! `gitaway __proxy-connect <host> <port>` is an internal helper that ssh
//! runs while gitaway's proxy is up (see [`crate::proxy::helper`]).
//! It's handled before anything else and isn't listed in the help.
//!
//! # Extending
//!
//! A new gitaway command: add it to [`args::Command`], its name to
//! [`OWN_COMMANDS`], and a module here that runs it. Remove it from
//! [`RESERVED_COMMANDS`] if it was reserved. A name planned for later can
//! be claimed early by adding it to [`RESERVED_COMMANDS`].

pub mod args;
pub mod away;
pub mod check_cmd;
pub mod key_cmd;
pub mod network;
pub mod profile_cmd;
pub mod prompt;
pub mod setup_cmd;
pub mod tunnel_access;
pub mod tunnel_cmd;
pub mod use_cmd;
pub mod web_guard;

use std::ffi::OsString;

use clap::Parser;

use crate::config::name::ProfileName;
use crate::config::paths::ConfigDir;
use crate::config::resolve::{Inputs, resolve};
use crate::config::store::Store;
use crate::error::{EXIT_ERROR, Error};
use crate::git::classify::{self, Kind};
use crate::git::repo::Repo;
use crate::git::runner::Git;
use crate::proxy;

/// gitaway's own commands. They take precedence over git aliases with the
/// same name.
pub const OWN_COMMANDS: &[&str] = &["setup", "profile", "use", "peer", "key", "check", "tunnel"];

/// Commands planned for later milestones. They're claimed now, so a git
/// alias with the same name doesn't silently change meaning when they land.
pub const RESERVED_COMMANDS: &[&str] = &[];

const HELP: &str = "\
gitaway: reach your self-hosted git server from anywhere

Usage: gitaway [--profile <name>] <command> [args]

gitaway's own commands:
  setup      Set up a profile from the VPN config your server gave this device
  check      Test a profile's tunnel and pin its home server's host key
  profile    Add, list, show, change and remove profiles
  use        Pin the current repository to a profile
  peer       Print what the WireGuard server needs to know about this device
  key        Replace or import this device's WireGuard key
  tunnel     Open your git server's web interface here, through the tunnel

Every other command goes to git. Network commands first check whether you're
at the remote's home network: clone, fetch, pull, push, ls-remote, remote
update/show/prune, remote add -f, remote set-head -a, submodule update/add and
archive --remote.

Options:
  --profile <name>   Use this profile (must come before any git options)
  -h, --help         Show this help
  --version          Show gitaway's version

Run `gitaway <command> --help` for a command's options.";

/// Runs gitaway with the process's arguments and returns the exit code.
/// The process is hardened first (see [`crate::hardening`]).
pub fn main() -> i32 {
    if let Err(e) = crate::hardening::harden_process() {
        // An extra layer, not the main protection, so it doesn't stop git.
        eprintln!(
            "gitaway: warning: couldn't protect gitaway's memory from other programs ({e}); \
             continuing without it"
        );
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gitaway: error: {e}");
            EXIT_ERROR
        }
    }
}

/// Runs gitaway with the given arguments (without the program name). Returns
/// the exit code for commands that finish here; commands that hand over to
/// git don't return on Unix.
pub fn run(args: Vec<OsString>) -> Result<i32, Error> {
    if args
        .first()
        .is_some_and(|first| first == proxy::helper::COMMAND)
    {
        let helper_args: Vec<String> = args[1..]
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        return Ok(proxy::helper::run(&helper_args));
    }
    let (profile_flag, args) = take_profile_flag(args)?;

    if args.is_empty() || matches_only(&args, &["-h", "--help"]) {
        println!("{HELP}");
        return Ok(0);
    }
    if matches_only(&args, &["--version"]) {
        println!("gitaway {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }

    let invocation = classify::classify(&args);
    match invocation.subcommand {
        Some(command) if OWN_COMMANDS.contains(&command) => run_own_command(
            &args[invocation.global_options.len()..],
            invocation.global_options,
            profile_flag.as_ref(),
        ),
        Some(command) if RESERVED_COMMANDS.contains(&command) => Err(Error::msg(format!(
            "`gitaway {command}` isn't available yet; it's planned for a later release"
        ))),
        _ if invocation.kind == Kind::Network => {
            match network::plan(&invocation, profile_flag.as_ref())? {
                network::Plan::Direct(git_args) => Err(Git::new(&[]).exec(&git_args).into()),
                network::Plan::Away(plan) => away::run(*plan),
            }
        }
        _ => {
            if profile_flag.is_some() {
                eprintln!("gitaway: note: --profile has no effect on local commands");
            }
            Err(Git::new(&[]).exec(&args).into())
        }
    }
}

/// The configuration store in the user's config directory.
pub(crate) fn open_store() -> Result<Store, Error> {
    Ok(Store::new(ConfigDir::from_env()?))
}

fn run_own_command(
    args: &[OsString],
    global_options: &[OsString],
    profile_flag: Option<&ProfileName>,
) -> Result<i32, Error> {
    let cli = match args::Cli::try_parse_from(
        std::iter::once(OsString::from("gitaway")).chain(args.iter().cloned()),
    ) {
        Ok(cli) => cli,
        // Prints help or the usage error, and exits with clap's code.
        Err(e) => e.exit(),
    };
    match cli.command {
        args::Command::Profile(command) => profile_cmd::run(command),
        args::Command::Setup(command) => setup_cmd::run(&open_store()?, command),
        args::Command::Use(command) => use_cmd::run(command, global_options),
        args::Command::Peer(command) => {
            let store = open_store()?;
            let name = pick_profile(&store, command.profile, profile_flag, global_options)?;
            key_cmd::peer(&store, &name)
        }
        args::Command::Key(args::KeyCommand::Rotate { profile, yes }) => {
            let store = open_store()?;
            let name = pick_profile(&store, profile, profile_flag, global_options)?;
            key_cmd::rotate(&store, &name, yes)
        }
        args::Command::Check(command) => {
            let store = open_store()?;
            let name = pick_profile(&store, command.profile, profile_flag, global_options)?;
            check_cmd::run(&store, &name, command.yes)
        }
        args::Command::Tunnel(command) => {
            let store = open_store()?;
            let name = pick_profile(&store, command.profile, profile_flag, global_options)?;
            tunnel_cmd::run(&store, &name, command.port.as_deref())
        }
        args::Command::Key(args::KeyCommand::Import { profile, yes }) => {
            let store = open_store()?;
            let name = pick_profile(&store, profile, profile_flag, global_options)?;
            key_cmd::import(&store, &name, yes)
        }
    }
}

/// The profile for a command that takes an optional profile name: the name
/// given, else `--profile`, `GITAWAY_PROFILE`, `git config gitaway.profile`
/// or `default_profile`, in that order. There's no remote to match, so the
/// `[hosts]` rule doesn't apply.
fn pick_profile(
    store: &Store,
    named: Option<String>,
    profile_flag: Option<&ProfileName>,
    global_options: &[OsString],
) -> Result<ProfileName, Error> {
    let named = named
        .map(|name| name.parse::<ProfileName>())
        .transpose()
        .map_err(|e| Error::msg(e.to_string()))?;
    let flag = named.as_ref().or(profile_flag);
    let env = network::env_profile()?;
    // git config is only a fallback here, so a git that can't run just means
    // it doesn't name a profile.
    let git_config = match flag.or(env.as_ref()) {
        Some(_) => None,
        None => Repo::new(Git::new(global_options))
            .profile()
            .ok()
            .flatten()
            .map(|name| name.parse::<ProfileName>())
            .transpose()
            .map_err(|e| {
                Error::msg(format!("git config {}: {e}", crate::git::repo::PROFILE_KEY))
            })?,
    };
    let global = store.load_global()?;
    let profiles = store.load_profiles()?;
    let inputs = Inputs {
        flag,
        env: env.as_ref(),
        git_config: git_config.as_ref(),
        remotes: &[],
        default: global.default_profile.as_ref(),
    };
    match resolve(inputs, &profiles, |_, _| false)? {
        Some(resolved) => Ok(resolved.name.clone()),
        None => Err(Error::msg(
            "no profile chosen: name one, or set a default with `gitaway profile default <name>`",
        )),
    }
}

/// Removes a leading `--profile <name>` or `--profile=<name>`.
fn take_profile_flag(
    mut args: Vec<OsString>,
) -> Result<(Option<ProfileName>, Vec<OsString>), Error> {
    let Some(first) = args.first().and_then(|a| a.to_str()) else {
        return Ok((None, args));
    };
    let (value, consumed) = if first == "--profile" {
        let value = args
            .get(1)
            .and_then(|v| v.to_str())
            .ok_or_else(|| Error::msg("--profile needs a profile name"))?;
        (value.to_owned(), 2)
    } else if let Some(value) = first.strip_prefix("--profile=") {
        (value.to_owned(), 1)
    } else {
        return Ok((None, args));
    };
    let name = value
        .parse()
        .map_err(|e| Error::msg(format!("--profile: {e}")))?;
    args.drain(..consumed);
    Ok((Some(name), args))
}

fn matches_only(args: &[OsString], options: &[&str]) -> bool {
    matches!(args, [only] if options.iter().any(|o| only == o))
}
