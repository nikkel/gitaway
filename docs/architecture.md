# Architecture

This guide is for people working on gitaway's code. For using gitaway, see the [README](../README.md) and the [configuration reference](configuration.md).

## The idea

gitaway wraps the `git` program; it doesn't reimplement git. It looks at the command line, decides whether the command needs the network, and if so works out whether you're at home. Then it runs your own `git` with your own config, credential helpers and SSH keys, and exits with git's exit code.

## Two programs, one codebase

gitaway is installed as two programs, `gitaway` and `git-away`, so that all of these work:

```console
$ gitaway push
$ git-away push
$ git away push
```

The last one works because of a git feature: when you type `git <name>` and `<name>` isn't a git command (or an alias), git looks on your `PATH` for a program called `git-<name>` and runs it with the remaining arguments. `git lfs` works the same way.

```text
you type:    git away push origin
git checks:  is "away" a git command or alias?   no
             is there a git-away on PATH?        yes
git runs:    git-away push origin
```

A few consequences:

- **Options before `away` belong to git.** In `git -C ~/code/app away push`, git applies `-C` itself, then runs `git-away push` in that folder. The result is the same as `gitaway -C ~/code/app push`.
- **`git away --help` is answered by git**, which opens `man git-away`. That's why a `git-away.1` man page is installed next to `gitaway.1` (see [Man pages](#man-pages)). `gitaway --help` and `git away -h` show gitaway's own short help.

In the code, both programs are tiny files that call the same function:

```text
src/main.rs           ──┐
                        ├──▶ gitaway::cli::main()   (src/cli/mod.rs: all the real work)
src/bin/git-away.rs   ──┘
```

Cargo treats `src/main.rs` as the binary named after the package, and every file in `src/bin/` as another binary named after the file. Both are built from the same library, so they can never behave differently.

Each binary contains a full copy of the code, a few megabytes each. That's deliberate: `cargo install` can only copy binaries, not create links, so two binaries is the only way it can install both names on every platform. Release archives for Linux and macOS will instead ship one binary plus a `git-away` symlink; the program doesn't care which name started it.

## What happens to a command

```text
gitaway [--profile <name>] [git global options] <command> [args]
│
├─ nothing, -h, --help ───────────────▶ gitaway's short help
├─ --version ─────────────────────────▶ gitaway's version
├─ profile, use, peer, key ───────────▶ gitaway's own commands
├─ list, tunnel ──────────────────────▶ "not available yet" (reserved)
├─ network command ───────────────────▶ choose profile ─▶ home check ─▶ git
│  (clone, fetch, pull, push, ls-remote,                       │
│   remote update, submodule update)                           └─ not home ─▶ error with the reason
└─ anything else ─────────────────────▶ git, exactly as typed
```

- gitaway's own commands take precedence over git aliases with the same name. The reserved names are claimed now, so an alias can't silently change meaning when those commands arrive.
- Classifying a command skips git's global options (`-C dir`, `-c key=value`, `--git-dir=…`) to find the subcommand. clap only ever sees gitaway's own commands, because it would reject git's options.
- For a network command, gitaway asks git which remote URL the command will use (`git remote get-url`, `git ls-remote --get-url`), then picks a profile using the order in the [configuration reference](configuration.md#which-profile-is-used).
- If the profile's `[hosts]` doesn't list the remote's host, git runs without a home check.
- If one of gitaway's own git lookups fails (a bad `-C` folder, a broken config file), the command goes to git as typed. The real command fails the same way before touching the network, so the user sees git's own message and exit code.
- On Linux and macOS, gitaway replaces itself with git (`exec`), so exit codes, signals and terminal behaviour are exactly git's. On Windows it runs git and waits.

## The home check

"Am I at home?" is answered by identity, not by address. An address match can be spoofed: a café network can use the same IP range as your home.

1. Connect to the profile's `[home].ssh` server, giving up after 200 ms.
2. Start an SSH key exchange, offering only the pinned key's type (a server with several host keys presents whichever type the client asks for). For RSA, only SHA-2 signatures are offered.
3. The server signs the exchange with its host key. The SSH library (`russh`) checks that signature before gitaway sees the key, so a server that only knows the public key can't pass.
4. Compare the key with the pinned `host_key`. The connection is closed without logging in.

The exchange is given 1 second. A match means home; anything else (a different key, a timeout, a refused connection) means away, with the reason kept for the error message. The code is in `src/home_check.rs`.

## Code map

| Path | Responsibility |
|---|---|
| `src/cli/` | The command line: dispatch (`mod.rs`), clap definitions (`args.rs`), prompts, and one module per command. `network.rs` decides what happens to a network command. |
| `src/git/` | Everything about git: classifying a command line, parsing remote URLs, reading a repository's remotes and config, and running git. |
| `src/config/` | Profile and config files: their format, validation, location, atomic writes, and which profile applies. |
| `src/home_check.rs` | The home check. |
| `src/keys/` | Device keys: generating them, and keeping them in the OS keychain or a private file. |
| `src/address.rs` | The `host[:port]` syntax, shared by remote URLs and profiles. |
| `src/error.rs` | The top-level error type and exit codes. |
| `xtask/` | Maintenance tasks (`cargo xtask …`), such as generating the man pages. Not part of the published crate. |
| `tests/cli.rs` | End-to-end tests of the real binary, with a fake `git` and a test SSH server. |

Each module starts with a `//!` comment that explains its purpose, how it fits in and how to extend it. `cargo doc --open` shows them all.

## Man pages

`cargo xtask man` generates the pages into `target/man/` (or `--out <dir>`). Read one with `man target/man/gitaway.1`.

- `gitaway.1` is written by hand in `xtask/src/man.rs`, because gitaway's top level isn't a clap command. A test fails if a gitaway command is missing from its command list.
- `git-away.1` is the same page under the name git opens for `git away --help`.
- Every gitaway command (`gitaway-profile-add.1`, …) gets a page generated from its clap definition, so its flags and help text can't drift from the code.

**Man pages are installed by packages, not by `cargo install`.** `man` only reads files from man directories; it can't ask a program for its page. `cargo install` can only copy binaries, so the release archives and the Homebrew formula generate the pages at build time and install them. The pages aren't committed, so there's no copy to keep in sync. CI generates them on every push, so a broken page is caught early. Users of `cargo install` use `--help` instead.

## Supported git versions

The minimum is git 2.34, the version in Ubuntu 22.04. CI runs the tests against it, against the latest git, and against Apple's git on macOS. Use only git commands and options that 2.34 supports.

## Extending

- **A gitaway command:** see the `# Extending` section at the top of `src/cli/mod.rs`.
- **A profile setting:** see `src/config/profile.rs`, and `src/cli/profile_cmd.rs` if `profile add` should ask for it. Update the [configuration reference](configuration.md) in the same change.
- **A network command:** add it to `NETWORK_COMMANDS` or `NETWORK_SUBCOMMANDS` in `src/git/classify.rs`, with a test.
