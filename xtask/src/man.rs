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
        "profile",
        "Add, list, show, change and remove profiles. See gitaway-profile(1).",
    ),
    (
        "use",
        "Pin the current repository to a profile. See gitaway-use(1).",
    ),
];

/// Writes every page into `dir`, replacing what's there.
pub fn write(dir: &Path) -> Result<(), String> {
    let pages = render_all()?;
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in &pages {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    println!("Wrote {} pages to {}", pages.len(), dir.display());
    Ok(())
}

/// Fails if `dir` doesn't hold exactly the pages `write` would produce.
pub fn check(dir: &Path) -> Result<(), String> {
    let expected = render_all()?;
    let mut problems = Vec::new();
    for (name, text) in &expected {
        match std::fs::read_to_string(dir.join(name)) {
            Ok(found) if &found == text => {}
            Ok(_) => problems.push(format!("{name} is out of date")),
            Err(_) => problems.push(format!("{name} is missing")),
        }
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let name = entry
            .map_err(|e| e.to_string())?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if !expected.contains_key(&name) {
            problems.push(format!("{name} isn't generated any more"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the man pages don't match the code. Run `cargo xtask man` and commit the result:\n  {}",
            problems.join("\n  ")
        ))
    }
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
        "gitaway wraps git. At home it runs git directly. Away from home it will \
         open a VPN tunnel that only gitaway uses, with no system-wide VPN and no \
         root. The tunnel isn't built yet: away from home, network commands stop \
         with a clear reason.",
    );
    page.para(
        "gitaway's own commands are listed below. Every other command goes to the \
         git on your PATH, unchanged, and gitaway exits with git's exit code.",
    );
    page.para(
        "Network commands (clone, fetch, pull, push, ls-remote, remote update and \
         submodule update) first choose a profile for the remote. If the profile \
         handles the remote's host, gitaway checks whether you're on that \
         profile's home network before running git.",
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
    page.item(
        "peer, key, list, tunnel",
        "Reserved for later releases. They report that they aren't available yet.",
    );
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
         the one whose home check passes; if that doesn't settle it, gitaway stops \
         and asks you to run gitaway use;",
        "default_profile in config.toml.",
    ]);
    page.para(
        "If the chosen profile's [hosts] doesn't list the remote's host, the \
         command goes to git without a home check. Pushing to a public forge is \
         unaffected.",
    );

    page.section("HOME CHECK");
    page.text(
        "gitaway connects to the SSH server in the profile's [home] section and \
         asks it to prove it holds the pinned host_key, as a normal SSH connection \
         does. Nothing logs in. If it proves it, you're home and git runs \
         directly. A different key, no answer within about a second, or a refused \
         connection all mean you're not home.",
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

    page.section("ENVIRONMENT");
    page.item(
        "GITAWAY_PROFILE",
        "The profile to use, unless --profile is given.",
    );
    page.item(
        "XDG_CONFIG_HOME",
        "Where the configuration directory lives.",
    );

    page.section("EXIT STATUS");
    page.text(
        "For a command run by git, git's exit status. Otherwise 0 on success, 1 \
         when gitaway reports an error, and 2 for a usage mistake in one of \
         gitaway's own commands.",
    );

    page.section("EXAMPLES");
    page.example(&[
        "gitaway profile add home",
        "gitaway clone ssh://git@192.0.2.10:2222/alice/app.git",
        "cd app && git away push",
        "gitaway use office",
    ]);

    page.section("SEE ALSO");
    page.text("gitaway-profile(1), gitaway-use(1), git(1), ssh-keyscan(1)");

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
