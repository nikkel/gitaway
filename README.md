# gitaway

![A house at home and a laptop at a café, joined by a git branch line that passes safely through a tunnel between them](assets/banner.png)

[![CI](https://github.com/nikkel/gitaway/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/nikkel/gitaway/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/gitaway.svg)](https://crates.io/crates/gitaway)
[![docs.rs](https://img.shields.io/docsrs/gitaway)](https://docs.rs/gitaway)
[![MSRV](https://img.shields.io/crates/msrv/gitaway)](https://crates.io/crates/gitaway)
[![License](https://img.shields.io/github/license/nikkel/gitaway)](#license)

Reach your self-hosted git server from anywhere. At home, `gitaway` connects directly. Away from home, it opens a VPN tunnel that only gitaway itself uses: no system-wide VPN, no virtual network adapter, no root.

> **Status:** early development. `0.1.0-alpha.1` is a pre-release for testing; 0.1.0 follows after a security review. Working today: guided setup from your server's WireGuard config, profiles, the home check, and the WireGuard tunnel away from home for `ssh://` and `https://` remotes.

## Why

You run Forgejo, Gitea or plain git over SSH on a machine at home, and your repositories' remotes point at its home address, such as `ssh://git@192.0.2.10:2222/alice/app.git`. At home that works. Elsewhere it doesn't, and the usual fixes all cost something:

- a full VPN on your laptop routes everything through home, needs admin rights, and must be switched on and off;
- a mesh VPN such as Tailscale means installing and trusting a third-party service;
- exposing the git server to the internet widens what attackers can reach.

gitaway takes a narrower path. Keep your remotes as they are, and use `git away push` instead of `git push`. At home nothing changes. Away, only that one git command goes through a tunnel to your home WireGuard server.

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
 away: open a WireGuard tunnel inside gitaway, run git through it,
       close it when git finishes
```

- **gitaway wraps your installed git.** Commands it doesn't own (`commit`, `log`, `status` …) go to git unchanged, and you get git's exit code. Your git config, credential helpers and SSH keys work as before.
- **Home is checked by identity, not by address.** A café network might use the same addresses as your home, but it can't produce your server's private host key. The check takes a few tens of milliseconds at home, and gives up within about a second elsewhere.
- **Profiles** describe each place you connect to. A repository remembers its profile, and remotes that no profile handles, such as GitHub, are left alone.
- **Away, git's URLs aren't changed.** gitaway runs a small proxy for that one command and points git at it, so certificates, credential helpers and `known_hosts` all keep working. Only the hosts listed in the profile go through the tunnel. The tunnel runs inside gitaway and closes when git finishes; nothing else on your machine is affected.

## Requirements

- **git 2.34 or newer** on your `PATH`.
- Linux or macOS. Windows should work but isn't tested as thoroughly.
- No OpenSSH for gitaway itself: the home check uses an SSH implementation built into gitaway. Your `ssh://` remotes still need an SSH client, as they do with plain git.

## Install

With [Rust](https://rustup.rs) 1.95 or newer, install the current pre-release from crates.io:

```console
$ cargo install gitaway --version 0.1.0-alpha.1 --locked
```

Or the latest code from GitHub:

```console
$ cargo install --git https://github.com/nikkel/gitaway --locked
```

This installs two commands, `gitaway` and `git-away`. They're the same program: git runs any program called `git-<name>` for `git <name>`, which is what makes `git away push` work. Use whichever you prefer.

On macOS, the first time a newly installed gitaway reads its device key, macOS asks for your login password to let it use the Keychain item. Choose **Always Allow**. It asks again after each update, because the permission belongs to that exact build of the program; signed release binaries (planned) will stop that.

`gitaway --help` and `git away -h` show the help. Man pages (`man gitaway`) will come with packaged installs, Homebrew and release archives, which are planned. `cargo install` can't install man pages, so with it, `git away --help` reports that there's no manual entry; use `git away -h` instead.

## Quick start

All addresses and keys here are examples.

**1. Add this device on your home WireGuard server.** WireGuard servers, router and NAS VPN apps, and tools such as wg-easy all have a way to add a device ("add client" or "add peer"), and give you a **config file** for it: the same file you'd import into the WireGuard app. It looks like this:

```ini
[Interface]
PrivateKey = <this device's private key>
Address = 10.0.0.2/32

[Peer]
PublicKey = <the server's public key>
Endpoint = vpn.example.net:51820
AllowedIPs = 10.0.0.1/32
```

It holds this device's private key, so treat it like a password. New to WireGuard? [WireGuard in five minutes](docs/wireguard-basics.md) explains what each line means.

**2. Run `gitaway setup`.** It asks for that file (its path, or paste the config itself) and for a clone URL from your git server, the way you use it at home, then works out everything else by trying it:

```console
$ gitaway setup
Name for this profile [home]:
The VPN config your server gave this device: type the file's path, or paste the config: laptop.conf
A clone URL from your git server, as you use it at home: ssh://git@192.0.2.10:2222/alice/app.git
Connecting to your VPN server at vpn.example.net:51820…
Looking for your git server inside the tunnel at 10.0.0.1:2222…
Tunnel: works.
Found your git server at 10.0.0.1:2222. Its host key: ssh-ed25519 SHA256:tAXFyTXI8xtDaujAEcwJslAYc9/6FKcUkd2Lw0xDhPo
gitaway will use it to recognise home.

Saved profile `home` (~/.config/gitaway/profiles/home.toml), with this device's key in the macOS Keychain.
`home` is now the default profile.
laptop.conf holds this device's private key, which gitaway has now stored, so you can delete the file.

Done. Try it: git away ls-remote ssh://git@192.0.2.10:2222/alice/app.git
```

| setup needs | It gets it from |
|---|---|
| your WireGuard server's address and public key, this device's tunnel address and private key | the config file |
| your git server's address at home | the clone URL |
| where your git server is inside the tunnel | trying the addresses the config's `AllowedIPs` point to |
| your git server's SSH host key, which is how gitaway recognises home | the git server itself, through the tunnel |

Nothing is saved unless the tunnel works, so if something's wrong you can fix it and run `setup` again.

**At home**, the tunnel usually can't be tested: from inside your network it would have to go out to your router's public address and back in, which many routers can't do. So when your git server answers on the local network, setup asks whether you're home. Say yes, and it saves the git server's key directly; then run `gitaway check` once from another network, such as your phone's hotspot, to test the tunnel.

**3. Use git through gitaway.**

```console
$ git away clone ssh://git@192.0.2.10:2222/alice/app.git
$ cd app
$ git away pull
$ git commit -am "fix"          # local commands work with or without gitaway
$ git away push
```

A clone through gitaway remembers its profile. For a repository you cloned before, run `gitaway use home` inside it.

Away from home, the same commands go through the tunnel. Nothing changes in how you use them. If something goes wrong, `gitaway check` tests the tunnel and says which part failed, and a failed git command is followed by gitaway's explanation, for example:

```text
gitaway: 192.0.2.10:2222: no answer from the VPN server at 198.51.100.7:51820 within 10 seconds. Check that the profile's server public key is right, that the server has this device's public key (`gitaway peer`), and that UDP port 51820 reaches it from here
```

### If your server wants this device's public key instead

Some servers don't hand out config files: you give them the device's public key. Then let gitaway make the key:

```console
$ gitaway profile add home \
    --endpoint vpn.example.net:51820 \
    --server-public-key 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=' \
    --address 10.0.0.2/32 \
    --home-ssh 192.0.2.10:2222 \
    --host 192.0.2.10=10.0.0.1
$ gitaway peer home        # the [Peer] block to add on the server
$ gitaway check home       # from another network: tests the tunnel and pins the host key
```

The [configuration reference](docs/configuration.md) explains each setting.

## Commands

| Command | Does |
|---|---|
| `gitaway setup [name]` | Create a profile from the VPN config your server gave this device and a clone URL, and test it. |
| `gitaway profile add [name]` | Create a profile setting by setting, asking for anything not given as a flag. |
| `gitaway profile list` | List profiles. The default is marked with `*`. |
| `gitaway profile show <name>` | Show a profile's file. |
| `gitaway profile set <name> <key> <value>` | Change one setting, keeping the file's comments. |
| `gitaway profile default <name>` | Make a profile the default. |
| `gitaway profile remove <name>` | Delete a profile and its device key. |
| `gitaway use <name>` | Pin the current repository to a profile. |
| `gitaway peer [name]` | Print the `[Peer]` block your WireGuard server needs for this device. |
| `gitaway check [name]` | Test the tunnel, pin or verify the home server's host key through it, and run the home check. |
| `gitaway key rotate [name]` | Replace the profile's device key. The server needs the new public key afterwards. |
| `gitaway key import [name]` | Use a private key the server or a WireGuard app already made, read from stdin: the key alone, or a whole client config. |
| anything else | Passed to git. |

`--profile <name>`, typed before the command, picks the profile for one command. Every command has `--help`, and once packaged installs exist, `man gitaway` will cover the rest. Planned: `list` (your repositories on the forge) and `tunnel` (keep the tunnel open for other tools).

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

- **The home check makes an SSH connection without logging in.** Tools such as fail2ban or CrowdSec, and the login protection built into many NAS systems, may count these as failed logins and ban your laptop's address. Allowlist your home network in them. Each check is a single connection, but a busy day of fetches adds up. A banned laptop looks exactly like being away (gitaway quietly uses the tunnel), so `gitaway check` points it out when the server answers through the tunnel but not directly.
- **A WireGuard server at home, reachable from the internet.** Any standard one works: `wg-quick` on a Linux box, your router's or NAS's VPN server, or a container. Add each device and give `gitaway setup` the config the server makes for it, or add the block `gitaway peer` prints.
- **The git server must be reachable from the WireGuard server's side of the tunnel.** If WireGuard runs on the git server itself, use the server's tunnel address (such as `10.0.0.1`) in `[hosts]`. If it runs elsewhere on your network and forwards to it, you can map the host to its own LAN address: `"192.0.2.10" = "192.0.2.10"`.

## Security model

- **What the home check proves:** that the server you're talking to holds the private key for the pinned host key. That's the same guarantee as SSH's `known_hosts`. It doesn't log in and sends no credentials.
- **Where the pinned key comes from matters.** If you capture it on a network an attacker controls, you could pin the attacker's key. `gitaway setup` and `gitaway check` capture it through the tunnel, where WireGuard has already authenticated the server. Captured on the home network instead (`setup` asks whether you're home first), it's only as trustworthy as that network, so run `gitaway check` from elsewhere once to confirm it.
- **Profiles hold no secrets.** They contain public keys and addresses.
- **Each profile has its own device key**, a WireGuard private key kept in your OS keychain: the macOS Keychain, the Windows Credential Manager, or the Secret Service (GNOME Keyring, KWallet) on Linux. On a machine without one, such as a headless server, it's kept in a file only you can read, and gitaway says so. Set `GITAWAY_KEY_STORE=file` to choose files. The key is never written to a profile, `~/.ssh`, logs, command lines or the environment, and only `gitaway key rotate` or `gitaway key import` replaces it. An imported key is read from stdin, never from the command line.
- **On Linux, gitaway can't be inspected while it runs.** It marks itself not dumpable, so no core dump holds the device key and other programs running as you can't attach a debugger or read its memory.
- **git authentication is unchanged.** gitaway never sees your SSH keys, passwords or tokens. git and ssh handle them as usual.
- **The tunnel lives inside gitaway, for one command.** No system VPN, network adapter, routes or root. The proxy git talks to listens only on `127.0.0.1` and needs a random token that changes every command. Only the profile's `[hosts]` go through the tunnel; anything else git contacts is connected directly, as git would. Two commands using the same profile take turns, because a WireGuard server keeps one session per device.

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## Limitations

- Remotes using an alias from `~/.ssh/config` only match a profile's `[hosts]` entry written with the same alias.
- Away from home, a `ProxyJump` or `ProxyCommand` inside your own ssh command (`GIT_SSH_COMMAND` or `core.sshCommand`) would route around the tunnel, so gitaway refuses it and shows how to move it to `~/.ssh/config`, where it still works.
- One command uses one profile's tunnel, so a command that reaches the home servers of two profiles is refused. Run it once per remote.
- Away from home, only `ssh://` and `https://` (or `http://`) remotes go through the tunnel. `git://` remotes don't: git can't hand them to gitaway's proxy, and Forgejo and Gitea don't serve `git://` anyway.
- HTTPS-only servers can't be home-checked yet: the check needs an SSH server on the home network.
- Only git commands that contact a remote are checked and tunnelled (`gitaway -h` lists them). Others go to git unchanged, as does anything git runs on a schedule outside gitaway, such as `git maintenance` prefetching.

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
