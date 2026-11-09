# gitaway

[![CI](https://github.com/nikkel/gitaway/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/nikkel/gitaway/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/gitaway.svg)](https://crates.io/crates/gitaway)
[![docs.rs](https://img.shields.io/docsrs/gitaway)](https://docs.rs/gitaway)
[![MSRV](https://img.shields.io/crates/msrv/gitaway)](https://crates.io/crates/gitaway)
[![License](https://img.shields.io/github/license/nikkel/gitaway)](#license)

Reach your self-hosted git server from anywhere. At home, `gitaway` connects directly. Away from home, it opens a VPN tunnel that only gitaway itself uses: no system-wide VPN, no virtual network adapter, no root.

> **Status:** early development, with no release yet. Working today: profiles, and checking whether you're at home before network commands. Away from home, network commands stop with a clear message. The tunnel is the next milestone.

## Why

You run Forgejo, Gitea or plain git over SSH on a machine at home, and your repositories' remotes point at its home address, such as `ssh://git@192.0.2.10:2222/alice/app.git`. At home that works. Elsewhere it doesn't, and the usual fixes all cost something:

- a full VPN on your laptop routes everything through home, needs admin rights, and must be switched on and off;
- a mesh VPN such as Tailscale means installing and trusting a third-party service;
- exposing the git server to the internet widens what attackers can reach.

gitaway takes a narrower path. Keep your remotes as they are, and use `git away push` instead of `git push`. At home nothing changes. Away, only that one git command goes through a tunnel to your home WireGuard server (from the next release).

## How it works

```text
git away push
     │
     ▼
 is this a network command for a server a profile handles?
     │ no ──────────────────────────────────▶ run git as typed
     │ yes
     ▼
 home check: does the home SSH server prove it holds the pinned host key?
     │ yes ─────────────────────────────────▶ run git directly
     │ no
     ▼
 away: open a WireGuard tunnel inside gitaway and point git at it   (next release)
```

- **gitaway wraps your installed git.** Commands it doesn't own (`commit`, `log`, `status` …) go to git unchanged, and you get git's exit code. Your git config, credential helpers and SSH keys work as before.
- **Home is checked by identity, not by address.** A café network might use the same addresses as your home, but it can't produce your server's private host key. The check takes a few milliseconds at home, and gives up within about a second elsewhere.
- **Profiles** describe each place you connect to. A repository remembers its profile, and remotes that no profile handles, such as GitHub, are left alone.

## Requirements

- **git 2.34 or newer** on your `PATH`.
- Linux or macOS. Windows should work but isn't tested as thoroughly.
- For the [quick start](#quick-start): `ssh-keyscan`, which comes with OpenSSH, to read your server's host key. There are [other ways](docs/configuration.md#home) to get it.

gitaway doesn't need OpenSSH itself: the home check uses an SSH implementation built into gitaway. Your `ssh://` remotes still need an SSH client, as they do with plain git.

## Install

From source, with [Rust](https://rustup.rs) 1.95 or newer:

```console
$ cargo install --git https://github.com/nikkel/gitaway
```

This installs two commands, `gitaway` and `git-away`. They're the same program: git runs any program called `git-<name>` for `git <name>`, which is what makes `git away push` work. Use whichever you prefer.

`gitaway --help` and `git away -h` show the help. The man pages (`man gitaway`) come with packaged installs: Homebrew and the release archives, from the first release. `cargo install` can't install man pages, so with it, `git away --help` reports that there's no manual entry; use `git away -h` instead.

## Quick start

All addresses here are examples. Use your own.

**1. Get your server's SSH host key.** On your home network:

```console
$ ssh-keyscan -p 2222 -t ed25519 192.0.2.10
# 192.0.2.10:2222 SSH-2.0-OpenSSH_9.6
[192.0.2.10]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop
```

You need the key line without the host name: `ssh-ed25519 AAAA…`. Ignore the `#` line that newer versions print first.

**2. Create a profile.** Run `gitaway profile add` and answer the questions, or give everything as flags:

```console
$ gitaway profile add home \
    --endpoint vpn.example.net:51820 \
    --server-public-key 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=' \
    --home-ssh 192.0.2.10:2222 \
    --host-key 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop' \
    --host 192.0.2.10=10.0.0.1 \
    --default
```

The `--endpoint` and `--server-public-key` are your home WireGuard server's public address and key. The `--host` entries list the servers this profile handles: the address in your remote URLs, and the server's address inside the tunnel.

This also creates the profile's own WireGuard key for this device, kept in your OS keychain.

**3. Tell your WireGuard server about this device.**

```console
$ gitaway peer home
[Peer]
# gitaway profile `home`
PublicKey = hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=
AllowedIPs = <a free tunnel address for this device>/32
```

Add the block to your server's WireGuard configuration with a free address in `AllowedIPs`, then save that address in the profile:

```console
$ gitaway profile set home transport.wireguard.address 10.0.0.2/32
```

The private key never leaves this device; only the public key goes to the server.

**4. Use git through gitaway.**

```console
$ git away clone ssh://git@192.0.2.10:2222/alice/app.git
$ cd app
$ git away pull
$ git commit -am "fix"          # local commands work with or without gitaway
$ git away push
```

A clone through gitaway remembers its profile. For a repository you cloned before, run `gitaway use home` inside it.

Away from home, network commands currently stop with:

```text
gitaway: error: not on the home network for profile `home` (192.0.2.10:2222: …).
Reaching it from elsewhere needs the tunnel, which isn't built yet
```

## Commands

| Command | Does |
|---|---|
| `gitaway profile add [name]` | Create a profile, asking for anything not given as a flag. |
| `gitaway profile list` | List profiles. The default is marked with `*`. |
| `gitaway profile show <name>` | Show a profile's file. |
| `gitaway profile set <name> <key> <value>` | Change one setting, keeping the file's comments. |
| `gitaway profile default <name>` | Make a profile the default. |
| `gitaway profile remove <name>` | Delete a profile and its device key. |
| `gitaway use <name>` | Pin the current repository to a profile. |
| `gitaway peer [name]` | Print the `[Peer]` block your WireGuard server needs for this device. |
| `gitaway key rotate [name]` | Replace the profile's device key. The server needs the new public key afterwards. |
| anything else | Passed to git. |

`--profile <name>`, typed before the command, picks the profile for one command. Every command has `--help`, and with a packaged install, `man gitaway` covers the rest. Planned: `list` (your repositories on the forge) and `tunnel` (keep the tunnel open for other tools).

## Configuration

Profiles are TOML files in `~/.config/gitaway/profiles/`:

```toml
[transport.wireguard]
endpoint = "vpn.example.net:51820"
server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

[home]
ssh = "192.0.2.10:2222"
host_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop"

[hosts]
"192.0.2.10" = "10.0.0.1"
```

The [configuration reference](docs/configuration.md) covers every setting, how a profile is chosen, and pinning a whole folder of repositories to a profile with `includeIf`.

## Server side

- **The home check makes an SSH connection without logging in.** Tools such as fail2ban or CrowdSec may count these as failed logins and eventually ban your laptop's address. Allowlist your home network in them. Each check is a single connection, so it rarely adds up, but a busy day of fetches can.
- The tunnel will need a WireGuard server at home, reachable from the internet. Setup instructions come with that release.

## Security model

- **What the home check proves:** that the server you're talking to holds the private key for the pinned host key. That's the same guarantee as SSH's `known_hosts`. It doesn't log in and sends no credentials.
- **Where the pinned key comes from matters.** If you capture it on a network an attacker controls, you could pin the attacker's key. Capture it at home, or read it from the server itself. (From the tunnel release, gitaway can capture it through the tunnel, where WireGuard has already authenticated the server.)
- **Profiles hold no secrets.** They contain public keys and addresses.
- **Each profile has its own device key**, a WireGuard private key kept in your OS keychain: the macOS Keychain, the Windows Credential Manager, or the Secret Service (GNOME Keyring, KWallet) on Linux. On a machine without one, such as a headless server, it's kept in a file only you can read, and gitaway says so. Set `GITAWAY_KEY_STORE=file` to choose files. The key is never written to a profile, `~/.ssh`, logs, command lines or the environment, and only `gitaway key rotate` replaces it.
- **git authentication is unchanged.** gitaway never sees your SSH keys, passwords or tokens. git and ssh handle them as usual.

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## Limitations

- No tunnel yet, so gitaway doesn't help away from home until the next release.
- Remotes using an alias from `~/.ssh/config` only match a profile's `[hosts]` entry written with the same alias.
- HTTPS-only servers can't be home-checked yet: the check needs an SSH server on the home network.

## Documentation

- [Configuration reference](docs/configuration.md)
- [Architecture](docs/architecture.md), for contributors
- [Contributing](CONTRIBUTING.md) and [changelog](CHANGELOG.md)

## Verifying releases

There are no releases yet. When there are, each one will be an [immutable GitHub release](https://docs.github.com/en/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases): once it's published, its tag and binaries can't be changed or replaced, not even by the maintainer. GitHub signs an attestation of each release's contents, which you can check with the [GitHub CLI](https://cli.github.com/):

```console
$ gh release verify v0.1.0 --repo nikkel/gitaway
$ gh release verify-asset v0.1.0 ./gitaway-x86_64-unknown-linux-gnu.tar.xz --repo nikkel/gitaway
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
