//! Run the user's installed `git`.
//!
//! [`Git`] carries the global options the user typed (such as `-C dir`), so
//! every git command gitaway runs, including its own config queries, sees the
//! same repository the user meant.
//!
//! Two ways to run git:
//!
//! - [`Git::exec`] hands over to git completely. On Unix the gitaway process
//!   *becomes* git (`exec`), so exit codes, signals, Ctrl+C and the terminal
//!   behave exactly as with plain git. Elsewhere it runs git, waits, and exits
//!   with git's code.
//! - [`Git::capture`] runs a query and returns its output, for gitaway's own
//!   use (reading `gitaway.profile`, remote URLs and so on).
//!
//! # Extending
//!
//! Milestone 2 adds a spawn-and-wait mode, so a tunnel can be torn down after
//! git exits. Use [`exit_code`] to turn git's status into gitaway's exit code.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, ExitStatus, Stdio};

/// git couldn't be run, or a query failed in an unexpected way.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// No `git` on `PATH`.
    #[error("git isn't installed or isn't on PATH")]
    NotFound,
    /// git exists but couldn't be started.
    #[error("couldn't run git: {0}")]
    Spawn(io::Error),
    /// A query exited with a code its caller doesn't expect.
    #[error("`git {command}` failed (exit code {code}): {stderr}")]
    Failed {
        /// The git arguments, for the message.
        command: String,
        /// git's exit code.
        code: i32,
        /// What git printed on stderr.
        stderr: String,
    },
    /// A query printed something that isn't UTF-8.
    #[error("`git {0}` printed output that isn't valid UTF-8")]
    NotUtf8(String),
}

/// The result of a git query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// git's exit code.
    pub code: i32,
    /// Standard output with the trailing newline removed.
    pub stdout: String,
    /// Standard error, for error messages.
    pub stderr: String,
}

/// How to run git: which program, with which global options and environment.
#[derive(Debug, Clone)]
pub struct Git {
    program: OsString,
    global_options: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl Git {
    /// `git` from `PATH`, with the global options the user typed before the
    /// subcommand (see [`super::classify::Invocation::global_options`]).
    pub fn new(global_options: &[OsString]) -> Self {
        Self {
            program: "git".into(),
            global_options: global_options.to_vec(),
            env: Vec::new(),
        }
    }

    /// Sets an environment variable for every git this runs. Tests use it to
    /// keep git away from the user's own config (`GIT_CONFIG_GLOBAL`).
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// A `Command` for `git <global options> <args>`.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.program);
        command
            .args(&self.global_options)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        command
    }

    /// Runs a git query and returns its output, whatever the exit code. The
    /// caller decides which codes mean what.
    pub fn capture(&self, args: &[&str]) -> Result<Captured, RunError> {
        let output = self
            .command(args)
            .stdin(Stdio::null())
            .output()
            .map_err(spawn_error)?;
        let describe = || args.join(" ");
        let stdout = String::from_utf8(output.stdout).map_err(|_| RunError::NotUtf8(describe()))?;
        Ok(Captured {
            code: exit_code(output.status),
            stdout: stdout.trim_end_matches(['\n', '\r']).to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    /// Hands over to `git <global options> <args>`. Returns only if git
    /// couldn't be started; otherwise the process ends with git's exit code.
    pub fn exec(&self, args: &[OsString]) -> RunError {
        let mut command = self.command(args);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Only returns on failure.
            spawn_error(command.exec())
        }
        #[cfg(not(unix))]
        {
            match command.status() {
                Ok(status) => std::process::exit(exit_code(status)),
                Err(e) => spawn_error(e),
            }
        }
    }
}

/// Turns a finished process's status into an exit code, the way shells do:
/// the process's own code, or 128 + the signal number if a signal killed it.
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

fn spawn_error(e: io::Error) -> RunError {
    if e.kind() == io::ErrorKind::NotFound {
        RunError::NotFound
    } else {
        RunError::Spawn(e)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn status_of(script: &str) -> ExitStatus {
        Command::new("sh").args(["-c", script]).status().unwrap()
    }

    #[test]
    fn exit_codes_pass_through() {
        assert_eq!(exit_code(status_of("exit 0")), 0);
        assert_eq!(exit_code(status_of("exit 42")), 42);
        assert_eq!(exit_code(status_of("exit 128")), 128);
    }

    #[test]
    fn signals_become_128_plus_the_signal() {
        assert_eq!(exit_code(status_of("kill -TERM $$")), 128 + 15);
        assert_eq!(exit_code(status_of("kill -KILL $$")), 128 + 9);
    }

    #[test]
    fn capture_returns_output_and_code() {
        let git = Git::new(&[]);
        let version = git.capture(&["--version"]).unwrap();
        assert_eq!(version.code, 0);
        assert!(version.stdout.starts_with("git version"));
        assert!(!version.stdout.ends_with('\n'));

        let bad = git.capture(&["not-a-git-command"]).unwrap();
        assert_ne!(bad.code, 0);
        assert!(!bad.stderr.is_empty());
    }

    #[test]
    fn global_options_are_applied() {
        let dir = tempfile::TempDir::new().unwrap();
        let git = Git::new(&["-C".into(), dir.path().into()]);
        let top = git
            .capture(&["rev-parse", "--is-inside-work-tree"])
            .unwrap();
        assert_ne!(top.code, 0, "an empty temp dir isn't a repository");
    }

    #[test]
    fn missing_git_is_reported_clearly() {
        let mut git = Git::new(&[]);
        git.program = "gitaway-test-no-such-program".into();
        assert!(matches!(
            git.capture(&["--version"]),
            Err(RunError::NotFound)
        ));
        assert!(matches!(git.exec(&[]), RunError::NotFound));
    }
}
