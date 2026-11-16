//! `gitaway completions <shell>`: prints a completion script.
//!
//! The script is generated from the same clap definitions as the help
//! text ([`super::args::Cli`]), so it can't drift from the commands and
//! flags. It completes gitaway's own commands only: everything else goes
//! to git, whose own completion handles it.

use std::io::Write;

use clap::CommandFactory;
use clap_complete::Shell;

use crate::error::Error;

use super::args::{Cli, CompletionsArgs};

/// Runs `gitaway completions`.
pub fn run(args: CompletionsArgs) -> Result<i32, Error> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&script(args.shell))
        .and_then(|()| stdout.flush())
        .map_err(|e| Error::msg(format!("couldn't write the script: {e}")))?;
    Ok(0)
}

/// The completion script for `shell`.
pub fn script(shell: Shell) -> Vec<u8> {
    let mut script = Vec::new();
    // `gitaway help` goes to git, so it isn't offered.
    let mut command = Cli::command().disable_help_subcommand(true);
    clap_complete::generate(shell, &mut command, "gitaway", &mut script);
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_completes_every_own_command() {
        for shell in [
            Shell::Bash,
            Shell::Elvish,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Zsh,
        ] {
            let script = String::from_utf8(script(shell)).unwrap();
            for command in super::super::OWN_COMMANDS {
                assert!(script.contains(command), "{shell}: no `{command}`");
            }
        }
    }
}
