# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
