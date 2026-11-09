# Configuration reference

gitaway's configuration is a few small TOML files. You can create and change them with `gitaway profile` commands, or edit them by hand. Every file is checked when it's loaded, and an unknown setting or a badly formatted value is an error that names the file, line and setting.

All values on this page are documentation examples. Use your own.

## Files

```text
~/.config/gitaway/            ($XDG_CONFIG_HOME/gitaway if that's set)
├── config.toml               settings that aren't per profile
├── profiles/
│   ├── home.toml             one file per profile
│   └── office.toml
└── keys/                     device keys, only where there's no OS keychain
    └── home.key
```

The same location is used on Linux, macOS and Windows. On Linux and macOS, gitaway writes files with mode 600 in directories with mode 700. It replaces a file in one step, so an interrupted write never leaves half a file.

## config.toml

```toml
default_profile = "home"      # optional
```

| Setting | Meaning |
|---|---|
| `default_profile` | The profile used when nothing more specific applies (see [Which profile is used](#which-profile-is-used)). Set it with `gitaway profile default <name>`. |

## Profiles

A profile is one place you connect to: your home, an office, a friend's server. The file name is the profile's name, which may contain lower-case letters, digits, `-` and `_`, can't start with `-`, and is at most 64 characters.

```toml
# ~/.config/gitaway/profiles/home.toml

[transport.wireguard]                 # how to reach home from elsewhere
endpoint = "vpn.example.net:51820"
server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
address = "10.0.0.2/32"               # optional
# mtu = 1420                          # optional

[home]                                # how to tell you're home
ssh = "192.0.2.10:2222"
host_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop"

[hosts]                               # which servers this profile handles
"192.0.2.10" = "10.0.0.1"
```

### `[transport.wireguard]`

How to reach the place when you're away. A profile has exactly one transport table.

| Setting | Required | Meaning |
|---|---|---|
| `endpoint` | yes | The WireGuard server's public address, as `host:port`. Usually a dynamic DNS name. |
| `server_public_key` | yes | The server's public key: 44 base64 characters, as printed by `wg pubkey`. In a client config, it's the `PublicKey` under `[Peer]`. |
| `address` | for the tunnel | This device's address inside the tunnel, such as `10.0.0.2/32`. Often assigned by the server's admin after setup, so it can be added later; until then, gitaway can't use the tunnel and says how to add it. |
| `mtu` | no | The tunnel MTU, at least 576. Leave it out unless the default causes trouble. |

The device's private key is never stored in a profile. See [Device keys](#device-keys).

### `[home]`

How gitaway tells that you're on this place's own network. Without it, gitaway can't tell, so it always uses the tunnel.

| Setting | Required | Meaning |
|---|---|---|
| `ssh` | yes | An SSH server on the home network, as `host` or `host:port` (port 22 if left out). Your git server's SSH port is the natural choice. |
| `host_key` | yes | That server's public host key, in OpenSSH's one-line format: `<type> <base64>`, optionally followed by a comment. Ed25519, ECDSA and RSA keys are accepted. |

To get the key, run this on the home network:

```console
$ ssh-keyscan -p 2222 -t ed25519 192.0.2.10
# 192.0.2.10:2222 SSH-2.0-OpenSSH_9.6
[192.0.2.10]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop
```

Copy only the key line, without the host name at its start: `ssh-ed25519 AAAA…`. Newer versions of `ssh-keyscan` print the `#` comment line first. Leave it out.

Safer still, read the key from the server's own file, `/etc/ssh/ssh_host_ed25519_key.pub`, or from your forge's admin settings. Then a device pretending to be your server can't hand you its own key.

How the check works is described in the [architecture guide](architecture.md#the-home-check).

### `[hosts]`

Which remotes this profile handles, and their addresses through the tunnel. Each key is a host as it appears in your remote URLs; each value is where to reach it through the tunnel.

```toml
[hosts]
"192.0.2.10" = "10.0.0.1"                 # any port on 192.0.2.10
"git.example.com" = "10.0.0.1"
"192.0.2.11:2222" = "10.0.0.3:22"         # only port 2222, mapped to port 22
```

- An entry is either host to host (the port is kept), or `host:port` to `host:port`. Mixing the two in one entry is an error. If a host has both, the `host:port` entry wins for that port.
- The tunnel side is where the VPN server can reach your git server. If both run on the same machine, that's the VPN server's own tunnel address (the `Address` in its WireGuard config, such as `10.0.0.1`). If the VPN server forwards traffic to your home network, the git server's home address works too (`"192.0.2.10" = "192.0.2.10"`). Your remote URLs keep using the home address either way.
- The tunnel side must be an IP address: names can't be looked up through the tunnel.
- Hosts are compared without regard to case. IPv6 addresses go in brackets: `"[2001:db8::10]" = "10.0.0.1"`.
- The host must be spelled the way your remote URL spells it. A remote that uses an alias from `~/.ssh/config` (`git@nas:alice/app.git`) only matches a key written as that alias.

A network command runs the home check only when its remote's host is listed here. Remotes that aren't listed, such as `github.com`, go straight to git, even when this profile is the default.

## Which profile is used

For a network command, gitaway uses the first of these that gives a profile:

1. `--profile <name>`, typed before the command: `gitaway --profile home push`.
2. The `GITAWAY_PROFILE` environment variable.
3. `git config gitaway.profile`, checked in the repository, then your global config, then any `includeIf` files. `gitaway use <name>` sets it for a repository. A `clone` run through gitaway sets it in the new repository automatically.
4. The one profile whose `[hosts]` lists the remote's host. If several do, gitaway runs each one's home check and uses the one that passes. If none or more than one passes, it stops and asks you to choose with `gitaway use`, because it never guesses.
5. `default_profile` in `config.toml`.

If none applies, the command goes to git unchanged.

The remote is the one named in the command (`git push upstream`), else the URL being cloned, else the remote git would use itself: the current branch's upstream, then `origin`.

Tip: to use one profile for every repository in a folder, put this in `~/.gitconfig`:

```ini
[includeIf "gitdir:~/code/home/"]
    path = ~/.config/git/home-profile.inc
```

and this in `~/.config/git/home-profile.inc`:

```ini
[gitaway]
    profile = home
```

## Changing a profile

`gitaway profile set` changes one setting and keeps the file's comments and layout. The result is checked before it's saved, so a mistake is rejected rather than written.

```console
$ gitaway profile set home transport.wireguard.address 10.0.0.2/32
$ gitaway profile set home transport.wireguard.mtu 1380
$ gitaway profile set home home.host_key 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop'
$ gitaway profile set home 'hosts."192.0.2.12"' 10.0.0.4
```

Keys are dotted paths. Quote a part that contains dots, as in the last example. Numbers and `true`/`false` are stored as such; anything else is stored as text.

## Device keys

Each profile has its own WireGuard private key for this device. `gitaway profile add` creates it, `gitaway peer` prints its public half for the server, `gitaway key rotate` replaces it, and `gitaway profile remove` deletes it.

If your server or a WireGuard app already made a key pair for this device, use that key instead with `gitaway key import`. It reads from stdin, so the key never appears in your shell history or the process list. Pipe in the key alone or a whole client config, which gitaway takes the `PrivateKey` line from:

```console
$ gitaway key import home < client.conf
```

In a terminal, run `gitaway key import home` and paste the key when asked; it isn't shown. Replacing an existing key asks first, or needs `--yes` when the key comes from a pipe. gitaway refuses the server's own private key. A key used by another device or app works, but only one of them can be connected at a time: the server keeps one session per key.

Keys are kept in the OS keychain: the macOS Keychain, the Windows Credential Manager, or the Secret Service on Linux (GNOME Keyring, KWallet), under the service name `gitaway` and the profile's name. Where there's no keychain, such as on a headless server, gitaway keeps the key in `keys/<profile>.key` in the configuration directory, with mode 600, and warns that it did. A key file other users can read is refused, as `ssh` refuses such files.

`GITAWAY_KEY_STORE` chooses explicitly:

| Value | Where keys go |
|---|---|
| unset | The keychain if there is one, otherwise a file |
| `keychain` | The keychain; an error if there isn't one |
| `file` | `keys/<profile>.key` |

Keychain entries are named after the profile only, so two configuration directories on one machine (with different `XDG_CONFIG_HOME` values) share keys for profiles with the same name. Use `GITAWAY_KEY_STORE=file` to keep them apart.

## Environment variables

| Variable | Meaning |
|---|---|
| `GITAWAY_PROFILE` | The profile to use, unless `--profile` is given. Empty means unset. |
| `XDG_CONFIG_HOME` | Where the configuration directory lives. Ignored unless it's an absolute path. |
| `GITAWAY_KEY_STORE` | Where device keys are kept: `keychain` or `file`. See [Device keys](#device-keys). |
