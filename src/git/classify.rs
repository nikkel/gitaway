//! Find the git subcommand in a command line and decide whether it needs the
//! network.
//!
//! git accepts global options before the subcommand (`git -C dir push`), and
//! some of them take a separate value (`-C dir`, `-c key=value`). To find the
//! subcommand reliably, [`classify`] skips them using the same rules as git.
//!
//! Only network commands go through the home check (and, later, the tunnel).
//! Everything else is passed straight to git.
//!
//! # Extending
//!
//! - A new network command: add it to [`NETWORK_COMMANDS`], or to
//!   [`NETWORK_SUBCOMMANDS`] if only one of its subcommands talks to a remote
//!   (like `remote update`).
//! - A new git global option: add it to [`GLOBAL_OPTIONS_WITH_VALUE`] if it can
//!   take its value as the next argument (`--opt value`). Options that only
//!   take `--opt=value`, or no value, need no change.

use std::ffi::{OsStr, OsString};

/// Commands that always talk to a remote.
pub const NETWORK_COMMANDS: &[&str] = &["clone", "fetch", "pull", "push", "ls-remote"];

/// Commands where only some subcommands talk to a remote, as
/// `(command, subcommand)` pairs.
pub const NETWORK_SUBCOMMANDS: &[(&str, &str)] = &[("remote", "update"), ("submodule", "update")];

/// git global options that can take their value as the next argument.
///
/// Taken from git's own option handling (`git.c`). Each also accepts the
/// `--opt=value` form, which needs no special handling here.
pub const GLOBAL_OPTIONS_WITH_VALUE: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--attr-source",
];

/// Whether a git command talks to a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// Works on the local repository only, such as `commit` or `status`.
    Local,
    /// Talks to a remote, such as `push` or `fetch`.
    Network,
}

/// A git command line split into its parts.
///
/// The slices borrow from the arguments passed to [`classify`], so nothing is
/// copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invocation<'a> {
    /// Global options before the subcommand, such as `-C dir`.
    pub global_options: &'a [OsString],
    /// The subcommand, such as `push`. `None` when there is none (`git
    /// --version`), when an unknown option comes first, or when it isn't
    /// valid UTF-8. git itself reports those cases.
    pub subcommand: Option<&'a str>,
    /// Everything after the subcommand.
    pub args: &'a [OsString],
    /// Whether the command talks to a remote.
    pub kind: Kind,
}

/// Splits git arguments (without the program name) and classifies the command.
///
/// # Examples
///
/// ```
/// use std::ffi::OsString;
/// use gitaway::git::classify::{classify, Kind};
///
/// let args: Vec<OsString> = ["-C", "app", "push", "origin"].map(OsString::from).into();
/// let invocation = classify(&args);
///
/// assert_eq!(invocation.subcommand, Some("push"));
/// assert_eq!(invocation.kind, Kind::Network);
/// assert_eq!(invocation.global_options.len(), 2);
/// ```
pub fn classify(args: &[OsString]) -> Invocation<'_> {
    let Some(index) = subcommand_index(args) else {
        return Invocation {
            global_options: args,
            subcommand: None,
            args: &[],
            kind: Kind::Local,
        };
    };

    let subcommand = args[index].to_str();
    let rest = &args[index + 1..];
    let kind = match subcommand {
        Some(command) if is_network(command, rest) => Kind::Network,
        _ => Kind::Local,
    };

    Invocation {
        global_options: &args[..index],
        subcommand,
        args: rest,
        kind,
    }
}

/// Returns the index of the subcommand, or `None` if git's global options run
/// out without one, or an unknown option comes first.
fn subcommand_index(args: &[OsString]) -> Option<usize> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        let Some(arg) = arg.to_str() else {
            // Non-UTF-8 can't be a known option. Treat it as the subcommand
            // position so git can report it.
            return Some(index);
        };
        if !arg.starts_with('-') {
            return Some(index);
        }
        if GLOBAL_OPTIONS_WITH_VALUE.contains(&arg) {
            index += 2;
        } else if is_known_global_flag(arg) {
            index += 1;
        } else {
            return None;
        }
    }
    None
}

/// Global options that take no separate value. An option written as
/// `--opt=value` also counts, because its value is part of the same argument.
fn is_known_global_flag(arg: &str) -> bool {
    const FLAGS: &[&str] = &[
        "-p",
        "--paginate",
        "-P",
        "--no-pager",
        "--bare",
        "--no-replace-objects",
        "--no-lazy-fetch",
        "--no-optional-locks",
        "--no-advice",
        "--literal-pathspecs",
        "--glob-pathspecs",
        "--noglob-pathspecs",
        "--icase-pathspecs",
        "--exec-path",
        "--html-path",
        "--man-path",
        "--info-path",
        "-v",
        "--version",
        "-h",
        "--help",
    ];
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    FLAGS.contains(&name) || (arg.contains('=') && GLOBAL_OPTIONS_WITH_VALUE.contains(&name))
}

fn is_network(command: &str, rest: &[OsString]) -> bool {
    if NETWORK_COMMANDS.contains(&command) {
        return true;
    }
    let Some(sub) = first_positional(rest) else {
        return false;
    };
    NETWORK_SUBCOMMANDS
        .iter()
        .any(|&(c, s)| c == command && OsStr::new(s) == sub)
}

/// The first argument that isn't an option, as in `git remote -v update`.
fn first_positional(args: &[OsString]) -> Option<&OsStr> {
    args.iter()
        .map(OsString::as_os_str)
        .find(|arg| !arg.as_encoded_bytes().starts_with(b"-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn network_commands_are_network() {
        for command in ["clone", "fetch", "pull", "push", "ls-remote"] {
            let args = os(&[command]);
            assert_eq!(classify(&args).kind, Kind::Network, "{command}");
        }
    }

    #[test]
    fn local_commands_are_local() {
        for command in ["commit", "status", "log", "remote", "submodule", "rebase"] {
            let args = os(&[command]);
            assert_eq!(classify(&args).kind, Kind::Local, "{command}");
        }
    }

    #[test]
    fn network_subcommands_are_found_past_options() {
        let args = os(&["remote", "-v", "update"]);
        assert_eq!(classify(&args).kind, Kind::Network);

        let args = os(&["submodule", "--quiet", "update", "--init"]);
        assert_eq!(classify(&args).kind, Kind::Network);
    }

    #[test]
    fn other_subcommands_of_mixed_commands_are_local() {
        let args = os(&["remote", "add", "update", "ssh://example.com/update.git"]);
        assert_eq!(classify(&args).kind, Kind::Local);

        let args = os(&["submodule", "status"]);
        assert_eq!(classify(&args).kind, Kind::Local);
    }

    #[test]
    fn global_options_with_separate_values_are_skipped() {
        let args = os(&["-C", "push", "-c", "user.name=alice", "status"]);
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, Some("status"));
        assert_eq!(invocation.kind, Kind::Local);
        assert_eq!(invocation.global_options, &args[..4]);
    }

    #[test]
    fn global_options_with_inline_values_are_skipped() {
        let args = os(&["--git-dir=x", "--work-tree=y", "--no-pager", "fetch"]);
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, Some("fetch"));
        assert_eq!(invocation.kind, Kind::Network);
    }

    #[test]
    fn arguments_after_the_subcommand_are_returned() {
        let args = os(&["-P", "push", "--force", "origin", "main"]);
        let invocation = classify(&args);
        assert_eq!(invocation.args, &args[2..]);
    }

    #[test]
    fn no_subcommand_is_local() {
        for args in [os(&[]), os(&["--version"]), os(&["-C", "dir"])] {
            let invocation = classify(&args);
            assert_eq!(invocation.subcommand, None);
            assert_eq!(invocation.kind, Kind::Local);
            assert!(invocation.args.is_empty());
        }
    }

    #[test]
    fn unknown_leading_option_stops_classification() {
        let args = os(&["--not-a-git-option", "push"]);
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, None);
        assert_eq!(invocation.kind, Kind::Local);
    }

    #[test]
    fn a_value_that_looks_like_a_command_is_not_the_subcommand() {
        let args = os(&["-c", "push", "log"]);
        assert_eq!(classify(&args).subcommand, Some("log"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_subcommand_is_passed_through_as_local() {
        use std::os::unix::ffi::OsStringExt;
        let args = vec![OsString::from("-P"), OsString::from_vec(vec![0xff, b'x'])];
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, None);
        assert_eq!(invocation.kind, Kind::Local);
        assert_eq!(invocation.global_options, &args[..1]);
    }
}
