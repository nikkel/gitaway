//! clap definitions for gitaway's own commands.
//!
//! The structs here only describe the command line: names, flags and help
//! text. The code that acts on them is in the `*_cmd` modules. Values are
//! kept as strings and validated by the command, so every error comes from
//! the same validation the profile file uses.

use clap::{Args, Parser, Subcommand};

/// gitaway's own commands.
#[derive(Debug, Parser)]
#[command(
    name = "gitaway",
    version,
    about = "Reach your self-hosted git server from anywhere"
)]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// A gitaway command.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Add, list, show, change and remove profiles.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Pin the current repository to a profile.
    Use(UseArgs),
}

/// `gitaway profile …`
#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Create a profile. Asks for anything not given as a flag.
    Add(AddArgs),
    /// List profiles. The default is marked with `*`.
    List,
    /// Show a profile's settings.
    Show {
        /// The profile to show.
        name: String,
    },
    /// Delete a profile.
    Remove {
        /// The profile to delete.
        name: String,
        /// Don't ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Make a profile the default, used when nothing more specific applies.
    Default {
        /// The profile to make the default.
        name: String,
    },
    /// Change one setting, keeping the file's comments and layout.
    ///
    /// Keys are dotted paths, such as `home.ssh` or
    /// `transport.wireguard.mtu`. Quote parts that contain dots:
    /// `'hosts."192.0.2.10"'`. The change is checked before it's saved.
    Set {
        /// The profile to change.
        name: String,
        /// The setting, as a dotted path.
        key: String,
        /// The new value. Numbers and `true`/`false` are read as such;
        /// anything else is a string.
        value: String,
    },
}

/// `gitaway profile add`
#[derive(Debug, Args)]
pub struct AddArgs {
    /// The profile's name: lower-case letters, digits, `-` and `_`.
    pub name: Option<String>,
    /// The WireGuard server's public address, as host:port.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// The WireGuard server's public key.
    #[arg(long)]
    pub server_public_key: Option<String>,
    /// This device's tunnel address, such as 10.0.0.2/32, if already known.
    #[arg(long)]
    pub address: Option<String>,
    /// The SSH server used to check you're home, as host:port.
    #[arg(long)]
    pub home_ssh: Option<String>,
    /// That SSH server's public host key (one line of `ssh-keyscan` output,
    /// without the host name).
    #[arg(long)]
    pub host_key: Option<String>,
    /// A server this profile handles, as LAN=TUNNEL (for example
    /// 192.0.2.10=10.0.0.1). Repeat for several.
    #[arg(long = "host", value_name = "LAN=TUNNEL")]
    pub hosts: Vec<String>,
    /// Make this the default profile.
    #[arg(long)]
    pub default: bool,
}

/// `gitaway use`
#[derive(Debug, Args)]
pub struct UseArgs {
    /// The profile to pin this repository to.
    pub profile: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn definitions_are_valid() {
        Cli::command().debug_assert();
    }
}
