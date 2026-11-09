# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Profiles check that each `[hosts]` entry's tunnel side is an IP address, since names can't be looked up through the tunnel.
- Device keys: `gitaway profile add` creates a WireGuard key for each profile, kept in the OS keychain (or a private file where there's none, or with `GITAWAY_KEY_STORE=file`). `gitaway peer` prints the `[Peer]` block for the server, and `gitaway key rotate` replaces the key.
- `gitaway` and `git-away` commands. Commands gitaway doesn't own are passed to git unchanged, with git's exit code.
- Profiles: `gitaway profile add`, `list`, `show`, `default`, `set` and `remove`, and `gitaway use` to pin a repository to a profile.
- Network commands (`clone`, `fetch`, `pull`, `push`, `ls-remote`, `remote update`, `submodule update`) check whether you're on the profile's home network by verifying its SSH server's pinned host key. Away from home they stop with a clear reason; the tunnel comes in a later release.
- Man pages, generated with `cargo xtask man`, for packaged installs: `man gitaway`, `man git-away` (opened by `git away --help`) and one page per gitaway command.
- Documentation: a README with a quick start and security model, a configuration reference and an architecture guide.
- Project scaffolding: crate layout, CI (format, lint, test, MSRV and secret scan), contributing guide and security policy.
