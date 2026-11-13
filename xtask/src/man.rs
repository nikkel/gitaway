//! Generates gitaway's man pages.
//!
//! - `gitaway.1` is written here by hand, in roff, because gitaway's top
//!   level isn't a clap command: anything that isn't one of gitaway's own
//!   commands goes to git.
//! - `git-away.1` is the same page, for `man git-away`. git opens it for
//!   `git away --help`, so it must exist under that name.
//! - One page per gitaway command (`gitaway-profile-add.1`, …) is generated
//!   from the clap definitions in `gitaway::cli::args`, so flags and help
//!   text can't drift from the code.
//!
//! # Extending
//!
//! A new gitaway command gets its page automatically. Mention it in
//! `COMMANDS` below so the main page lists it.

use std::collections::BTreeMap;
use std::path::Path;

use clap::CommandFactory;
use gitaway::cli::args::Cli;
use roff::{Roff, bold, italic, roman};

/// The commands listed in the main page, with a one-line summary.
const COMMANDS: &[(&str, &str)] = &[
    (
        "setup",
        "Set up a profile from the VPN config your server gave this device and a clone URL. \
         See gitaway-setup(1).",
    ),
    (
        "profile",
        "Add, list, show, change and remove profiles. See gitaway-profile(1).",
    ),
    (
        "use",
        "Pin the current repository to a profile. See gitaway-use(1).",
    ),
    (
        "peer",
        "Print the [Peer] block the WireGuard server needs for this device. See gitaway-peer(1).",
    ),
    (
        "check",
        "Test the tunnel, and pin or verify the home server's host key through it. See gitaway-check(1).",
    ),
    (
        "key",
        "Replace this device's WireGuard key with key rotate, or use an existing one with key \
         import. See gitaway-key-rotate(1) and gitaway-key-import(1).",
    ),
];

/// Writes every page into `dir`, creating it if needed. Existing files
/// with the same names are replaced; nothing else in `dir` is touched.
pub fn write(dir: &Path) -> Result<(), String> {
    let pages = render_all()?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in &pages {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    println!("Wrote {} pages to {}", pages.len(), dir.display());
    Ok(())
}

/// Every page, by file name.
fn render_all() -> Result<BTreeMap<String, String>, String> {
    let mut pages = BTreeMap::new();
    pages.insert("gitaway.1".to_owned(), main_page());
    pages.insert("git-away.1".to_owned(), main_page());

    let mut cli = Cli::command().disable_help_subcommand(true);
    cli.build();
    for command in cli.get_subcommands() {
        add_command_pages(command, &mut pages)?;
    }
    Ok(pages)
}

/// A page for `command` and each of its subcommands.
fn add_command_pages(
    command: &clap::Command,
    pages: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    for sub in command.get_subcommands() {
        add_command_pages(sub, pages)?;
    }
    let man = clap_mangen::Man::new(command.clone())
        .source(format!("gitaway {}", env!("CARGO_PKG_VERSION")))
        .manual("gitaway manual");
    let mut out = Vec::new();
    man.render(&mut out).map_err(|e| e.to_string())?;
    let text = String::from_utf8(out).map_err(|e| e.to_string())?;
    pages.insert(man.get_filename(), text);
    Ok(())
}

/// `gitaway.1`.
fn main_page() -> String {
    let mut page = Page::default();
    page.roff.control(
        "TH",
        [
            "GITAWAY",
            "1",
            "",
            &format!("gitaway {}", env!("CARGO_PKG_VERSION")),
            "gitaway manual",
        ],
    );

    page.section("NAME");
    page.text("gitaway, git-away - reach your self-hosted git server from anywhere");

    page.section("SYNOPSIS");
    page.roff.text([
        bold("gitaway"),
        roman(" [--profile "),
        italic("name"),
        roman("] "),
        italic("command"),
        roman(" ["),
        italic("args"),
        roman("]"),
    ]);
    page.roff.control("br", []);
    page.roff.text([
        bold("git away"),
        roman(" "),
        italic("command"),
        roman(" ["),
        italic("args"),
        roman("]"),
    ]);

    page.section("DESCRIPTION");
    page.text(
        "gitaway wraps git. At home it runs git directly. Away from home it opens \
         a WireGuard tunnel inside gitaway, with no system-wide VPN and no root, \
         runs git through it, and closes it when git finishes.",
    );
    page.para(
        "gitaway's own commands are listed below. Every other command goes to the \
         git on your PATH, unchanged, and gitaway exits with git's exit code.",
    );
    page.para(
        "Network commands (clone, fetch, pull, push, ls-remote, remote update, \
         remote show, remote prune, remote add with -f, remote set-head with -a, \
         submodule update, submodule add, and archive with --remote) first choose \
         a profile for the servers they may contact. If the profile handles any \
         of them, gitaway checks whether you're on that profile's home network \
         before running git.",
    );
    page.para(
        "The same program is installed as gitaway and git-away. git runs any \
         program named git-<name> on your PATH for git <name>, so \
         git away push and gitaway push do the same thing.",
    );

    page.section("COMMANDS");
    for (name, summary) in COMMANDS {
        page.item(name, summary);
    }
    // Read from the code, so the page can't drift from what gitaway refuses.
    let reserved = gitaway::cli::RESERVED_COMMANDS;
    match reserved.len() {
        0 => {}
        1 => page.item(
            reserved[0],
            "Reserved for a later release. It reports that it isn't available yet.",
        ),
        _ => page.item(
            &reserved.join(", "),
            "Reserved for later releases. They report that they aren't available yet.",
        ),
    }
    page.item("anything else", "Passed to git unchanged.");

    page.section("OPTIONS");
    page.item(
        "--profile name",
        "Use this profile for a network command. It must come before any git options.",
    );
    page.item(
        "-h, --help",
        "Show a short help. git away --help opens this page.",
    );
    page.item("--version", "Show gitaway's version.");
    page.para(
        "git's global options, such as -C dir and -c key=value, may come before the \
         command and are passed on to git.",
    );

    page.section("PROFILES");
    page.text(
        "A profile is one place you connect to, such as a home or an office. Each \
         is a TOML file in the profiles directory. For a network command, gitaway \
         uses the first of:",
    );
    page.numbered(&[
        "the --profile option;",
        "the GITAWAY_PROFILE environment variable;",
        "git config gitaway.profile, which gitaway use and gitaway clone set;",
        "the only profile whose [hosts] lists the remote's host. If several do, \
         the one whose home check passes, else default_profile if it's one of \
         them; if that doesn't settle it, gitaway stops and asks you to choose \
         with gitaway use or --profile;",
        "default_profile in config.toml.",
    ]);
    page.para(
        "If the chosen profile's [hosts] doesn't list the remote's host, the \
         command goes to git without a home check. Pushing to a public forge is \
         unaffected.",
    );

    page.section("DEVICE KEYS");
    page.text(
        "Each profile has its own WireGuard private key, created by gitaway profile add \
         and kept in the OS keychain (the macOS Keychain, the Windows Credential \
         Manager or the Secret Service on Linux). Where there's no keychain, it's kept \
         in a file readable only by you. Only gitaway key rotate and gitaway key \
         import replace it, and gitaway peer prints the public half for the server.",
    );

    page.section("HOME CHECK");
    page.text(
        "gitaway connects to the SSH server in the profile's [home] section and \
         asks it to prove it holds the pinned host_key, as a normal SSH connection \
         does. Nothing logs in. If it proves it, you're home and git runs \
         directly. A different key, no answer within about a second, a refused \
         connection, or no pinned key all mean you're not home. gitaway check pins \
         the key through the tunnel, where WireGuard has already authenticated the \
         server.",
    );

    page.section("AWAY FROM HOME");
    page.text(
        "gitaway opens the profile's WireGuard tunnel inside its own process, starts a \
         proxy on 127.0.0.1 for this one command, and runs git pointed at it: ssh \
         through a ProxyCommand, http and https through http.proxy. Remote URLs are \
         not changed. Hosts in the profile's [hosts] go through the tunnel; anything \
         else git contacts is connected directly. git:// remotes can't use the tunnel.",
    );
    page.para(
        "Ctrl+C reaches git as usual; gitaway waits for git, closes the tunnel and \
         exits with git's exit status. Two commands using the same profile take turns.",
    );

    page.section("FILES");
    page.item(
        "$XDG_CONFIG_HOME/gitaway/config.toml",
        "Settings that aren't per profile: default_profile. Without \
         XDG_CONFIG_HOME, ~/.config is used.",
    );
    page.item(
        "$XDG_CONFIG_HOME/gitaway/profiles/name.toml",
        "One file per profile. Written with mode 600 in a directory with mode 700.",
    );
    page.item(
        "$XDG_CONFIG_HOME/gitaway/keys/name.key",
        "A profile's device key, only when it isn't kept in the OS keychain. Mode 600; \
         a key file other users can read is refused.",
    );

    page.section("ENVIRONMENT");
    page.item(
        "GITAWAY_PROFILE",
        "The profile to use, unless --profile is given.",
    );
    page.item(
        "XDG_CONFIG_HOME",
        "Where the configuration directory lives.",
    );
    page.item(
        "GITAWAY_KEY_STORE",
        "Where device keys are kept: keychain (the OS keychain) or file (the keys \
         directory). Unset means the keychain if there is one, else a file.",
    );

    page.section("EXIT STATUS");
    page.text(
        "For a command run by git, git's exit status. Otherwise 0 on success, 1 \
         when gitaway reports an error, and 2 for a usage mistake in one of \
         gitaway's own commands.",
    );

    page.section("EXAMPLES");
    page.example(&[
        "gitaway setup",
        "gitaway clone ssh://git@192.0.2.10:2222/alice/app.git",
        "cd app && git away push",
        "gitaway use office",
    ]);

    page.section("SEE ALSO");
    page.text(
        "gitaway-setup(1), gitaway-profile(1), gitaway-use(1), gitaway-peer(1), gitaway-check(1), \
         gitaway-key-rotate(1), gitaway-key-import(1), git(1), ssh-keyscan(1)",
    );

    page.roff.to_roff()
}

/// Small helpers that keep `main_page` readable.
#[derive(Default)]
struct Page {
    roff: Roff,
}

impl Page {
    fn section(&mut self, title: &str) {
        self.roff.control("SH", [title]);
    }

    fn text(&mut self, text: &str) {
        self.roff.text([roman(text)]);
    }

    fn para(&mut self, text: &str) {
        self.roff.control("PP", []);
        self.text(text);
    }

    fn item(&mut self, term: &str, text: &str) {
        self.roff.control("TP", []);
        self.roff.text([bold(term)]);
        self.text(text);
    }

    fn numbered(&mut self, items: &[&str]) {
        for (i, item) in items.iter().enumerate() {
            self.roff
                .control("IP", [format!("{}.", i + 1).as_str(), "4"]);
            self.text(item);
        }
    }

    fn example(&mut self, lines: &[&str]) {
        self.roff.control("PP", []);
        self.roff.control("nf", []);
        self.roff.control("RS", ["4"]);
        for line in lines {
            self.roff.text([roman(format!("$ {line}"))]);
        }
        self.roff.control("RE", []);
        self.roff.control("fi", []);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_listed_on_the_main_page() {
        let listed: Vec<&str> = COMMANDS.iter().map(|(name, _)| *name).collect();
        for command in Cli::command().get_subcommands() {
            assert!(
                listed.contains(&command.get_name()),
                "add `{}` to COMMANDS",
                command.get_name()
            );
        }
    }

    #[test]
    fn every_command_has_a_page_and_git_away_has_one_too() {
        let pages = render_all().unwrap();
        for name in [
            "gitaway.1",
            "git-away.1",
            "gitaway-use.1",
            "gitaway-profile.1",
            "gitaway-profile-add.1",
            "gitaway-peer.1",
            "gitaway-key-rotate.1",
            "gitaway-key-import.1",
            "gitaway-check.1",
            "gitaway-setup.1",
        ] {
            assert!(pages.contains_key(name), "{name} missing");
        }
        assert_eq!(pages["gitaway.1"], pages["git-away.1"]);
    }

    #[test]
    fn the_main_page_names_exactly_the_reserved_commands() {
        let page = &render_all().unwrap()["gitaway.1"];
        for name in gitaway::cli::RESERVED_COMMANDS {
            assert!(page.contains(name), "`{name}` missing from gitaway.1");
        }
        assert!(
            !page.contains("list, tunnel"),
            "gitaway.1 still lists `list` as reserved"
        );
    }
}
