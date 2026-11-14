# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A Homebrew tap: `brew install nikkel/tap/gitaway` installs the prebuilt binary, the `git-away` command and the man pages on macOS and Linux. It follows each new release within a day.
- `gitaway tunnel`: opens your git server's web interface on this computer, through the tunnel, until Ctrl+C. It serves the profile's new `[web] url` on `http://localhost:<port>`, asking for the address the first time; at home it just prints the address to open. The tunnel sends WireGuard keepalives while it's open. It only forwards requests addressed to `localhost` (or `127.0.0.1`, `[::1]`) on its port, so web pages can't reach your server through it by DNS rebinding. While it runs, other gitaway commands for the same profile wait for it to stop.

### Changed

- `list` is no longer reserved: the planned command that listed your repositories through the git server's API is dropped, since it needed an API token on every device. `gitaway list` now goes to git, so a git alias named `list` works. `gitaway tunnel` lets you use the git server's own web interface instead.

## [0.1.1] - 2026-11-13

The first release with prebuilt binaries. Changes since 0.1.0:

### Added

- Prebuilt binaries for Linux (x86_64 and arm64, static), macOS (Intel and Apple Silicon) and Windows (x86_64), with install scripts for Linux, macOS and Windows. Each release file has a SHA-256 checksum and a GitHub artifact attestation, and each release is immutable. See the README for how to install and verify them. They aren't signed by Apple yet.

### Security

- gitaway's configuration directory is made private (mode 700) when gitaway writes to it, even if something else created it first. Before, a directory created by another program, such as an installer, kept its permissions, so other users on the machine could see which files it held, though not read them.

## [0.1.1-rc.1] - 2026-11-12

A GitHub pre-release to test the binaries and their installers. It wasn't published to crates.io. Its changes are listed under 0.1.1.

## [0.1.0] - 2026-11-11

The first release, after security, functionality and runtime reviews. Changes since 0.1.0-alpha.1:

### Security

- On Linux, gitaway marks itself not dumpable while it runs: no core dump can write the device key to disk, and other programs running as you can't attach a debugger or read its memory. git and ssh, which gitaway starts, are unaffected. If the system refuses, gitaway warns and carries on.
- `gitaway setup --config` no longer repeats its value in an error when that value is a key, or a line of a config, rather than a path.
- Away from home, a `[hosts]` entry written as an ssh alias now goes through the tunnel even when `~/.ssh/config` gives the alias a different `Hostname`. Before, ssh connected to that address directly.
- A command that contacts several servers (a remote with several push URLs, `fetch --all`, `remote update`, submodules) now uses the tunnel if any of them is a home server. Before, only the first URL was checked, and the others could be reached directly. A command reaching two profiles' home servers is refused.
- Your own `http.<url>.proxy` or `remote.<name>.proxy` can no longer send a home URL around the tunnel: gitaway outranks them for home URLs, for that command only. A `ProxyJump` or `ProxyCommand` in your own ssh command is refused away from home, with how to move it to `~/.ssh/config`.
- More commands that contact a remote now go through the home check and the tunnel: `submodule add`, `remote show`, `remote prune`, `remote add -f`, `remote set-head -a` and `archive --remote`. Before, they went straight to git, so away from home they reached the home server directly.
- A proxy given for one command (`-c`, `--config-env` or `GIT_CONFIG_PARAMETERS`) that would outrank gitaway's for a home URL is refused away from home, with an explanation.

### Changed

- When several profiles handle a remote and no home check settles it, gitaway uses your default profile if it's one of them, instead of stopping. That happens when one server is set up twice, say once for ssh and once for https.
- `gitaway check` away from home ends with a single line about firewalls and security tools, instead of a paragraph.

### Fixed

- On Linux, the tunnel's receive path once again splits the UDP datagrams the kernel merges (GRO), which it had stopped doing since the Windows fix in 0.1.0-alpha.1. Merged datagrams were dropped, which could stall large transfers.
- `gitaway setup` takes a pasted config that starts with a `#` comment line, instead of reading that line as a file path.
- On Windows, an ICMP "time exceeded" report for a packet the tunnel sent no longer stops the tunnel from receiving, as "port unreachable" reports already didn't.

## [0.1.0-alpha.1] - 2026-11-10

A pre-release for testing. It isn't installed unless asked for by version, and it hasn't had the security review that comes before 0.1.0.

### Added

- WireGuard preshared keys: a `PresharedKey` in a client config is used by `gitaway setup` and `gitaway key import`, and kept with the device key in the keychain (one entry, so macOS asks for permission once). `key rotate` keeps it. `profile show` says when a profile has one.
- `gitaway setup` creates a working profile from two answers: the VPN config file the server gave this device (a WireGuard `.conf`) and a clone URL from the git server. It finds the git server inside the tunnel by trying the addresses the config routes, pins its host key through the tunnel, and saves nothing unless that works. At home, where the tunnel often can't be reached, it asks whether you're home and pins the key directly.
- `docs/wireguard-basics.md` explains WireGuard's keys, peers and config files for people new to it. The quick start now starts with `gitaway setup`.
- `examples/home/`: a WireGuard server and Forgejo in Docker, for trying gitaway without a home server, and an end-to-end test (`e2e.sh`) that CI runs against them. Its WireGuard server hands out device configs (`new-device.sh`), and the test sets up with `gitaway setup`.
- `gitaway check` tests a profile's tunnel end to end, pins the home server's SSH host key through it (safe from any network, no `ssh-keyscan` at home needed), warns if a pinned key has changed, and runs the home check. `[home].host_key` is now optional: until it's pinned, commands use the tunnel.
- `gitaway key import` uses a private key the server or a WireGuard app already made, read from stdin (the key alone or a whole client config). It refuses the server's own key and asks before replacing a key.
- `gitaway profile add` ends by printing the `[Peer]` block for the server. The tunnel refuses a profile whose server key is this device's own key, a common mix-up, and says how to fix it.
- The tunnel: away from home, network commands run git through a WireGuard tunnel inside gitaway (no system VPN, adapter or root), via a per-command proxy, so remote URLs, certificates, credential helpers and `known_hosts` are unchanged. `ssh://` and `https://` remotes are supported. Commands on the same profile take turns. If the tunnel fails, gitaway says whether the VPN server never answered or the server behind it didn't. When the VPN server's name has both kinds of address, IPv4 is used.
- Profiles check that each `[hosts]` entry's tunnel side is an IP address, since names can't be looked up through the tunnel.
- Device keys: `gitaway profile add` creates a WireGuard key for each profile, kept in the OS keychain (or a private file where there's none, or with `GITAWAY_KEY_STORE=file`). `gitaway peer` prints the `[Peer]` block for the server, and `gitaway key rotate` replaces the key.
- `gitaway` and `git-away` commands. Commands gitaway doesn't own are passed to git unchanged, with git's exit code.
- Profiles: `gitaway profile add`, `list`, `show`, `default`, `set` and `remove`, and `gitaway use` to pin a repository to a profile.
- Network commands (`clone`, `fetch`, `pull`, `push`, `ls-remote`, `remote update`, `submodule update`) check whether you're on the profile's home network by verifying its SSH server's pinned host key. Away from home they go through the profile's WireGuard tunnel.
- Man pages, generated with `cargo xtask man`, for packaged installs: `man gitaway`, `man git-away` (opened by `git away --help`) and one page per gitaway command.
- Documentation: a README with a quick start and security model, a configuration reference and an architecture guide.
- Project scaffolding: crate layout, CI (format, lint, test, MSRV and secret scan), contributing guide and security policy.

[Unreleased]: https://github.com/nikkel/gitaway/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/nikkel/gitaway/compare/v0.1.0...v0.1.1
[0.1.1-rc.1]: https://github.com/nikkel/gitaway/compare/v0.1.0...v0.1.1-rc.1
[0.1.0]: https://github.com/nikkel/gitaway/compare/v0.1.0-alpha.1...v0.1.0
[0.1.0-alpha.1]: https://github.com/nikkel/gitaway/releases/tag/v0.1.0-alpha.1
