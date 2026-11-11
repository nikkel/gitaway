# Configuration reference

gitaway's configuration is a few small TOML files. `gitaway setup` writes a working profile from the VPN config your server gives a device; you can also create and change profiles with `gitaway profile` commands, or edit them by hand. Every file is checked when it's loaded, and an unknown setting or a badly formatted value is an error that names the file, line and setting.

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
| `host_key` | to recognise home | That server's public host key, in OpenSSH's one-line format: `<type> <base64>`, optionally followed by a comment. Ed25519, ECDSA and RSA keys are accepted. Until it's set, gitaway can't recognise home, so it always uses the tunnel. |

The easiest way to set the key is `gitaway check`, away from home, once the tunnel works:

```console
$ gitaway check home
```

It opens the tunnel, reaches the `ssh` server through it (at its `[hosts]` address), and has the server prove its host key. The tunnel is authenticated by WireGuard, so this is your server's real key even on an untrusted network. It shows the key's fingerprint and asks before pinning it. If a key is already pinned, it confirms it still matches; if it doesn't, it explains the possible reasons and asks before replacing it. `--yes` answers yes for scripts. At home, with a key already pinned, it checks the key directly instead, because many routers can't reach their own public address from inside the network.

Or, on the home network, get the key with `ssh-keyscan` and pass it to `profile add --host-key` or `profile set`:

```console
$ ssh-keyscan -p 2222 -t ed25519 192.0.2.10
# 192.0.2.10:2222 SSH-2.0-OpenSSH_9.6
[192.0.2.10]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop
```

Copy only the key line, without the host name at its start: `ssh-ed25519 AAAA…`. Newer versions of `ssh-keyscan` print the `#` comment line first. Leave it out.

On a LAN, `ssh-keyscan` trusts whatever answers. Reading the key from the server's own file, `/etc/ssh/ssh_host_ed25519_key.pub`, or from your forge's admin settings avoids that, as `gitaway check` does.

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
- The host must be spelled the way your remote URL spells it. A remote that uses an alias from `~/.ssh/config` (`git@nas:alice/app.git`) only matches a key written as that alias. Away from home, that alias goes through the tunnel even when `~/.ssh/config` gives it a different `Hostname` or `Port`. Use a host-only key for an alias (`"nas" = "10.0.0.1"`), since ssh may connect on the port from its config.

A network command runs the home check only when one of the hosts it may contact is listed here. That includes every push URL of the remote, every remote for `fetch --all` and `remote update`, and the repository's submodules. If any of them is a home host, the whole command uses the proxy, which still connects to the others directly. A command that would reach the home hosts of two different profiles is refused, because it can only use one tunnel. Remotes that aren't listed, such as `github.com`, go straight to git, even when this profile is the default.

## Which profile is used

For a network command, gitaway uses the first of these that gives a profile:

1. `--profile <name>`, typed before the command: `gitaway --profile home push`.
2. The `GITAWAY_PROFILE` environment variable.
3. `git config gitaway.profile`, checked in the repository, then your global config, then any `includeIf` files. `gitaway use <name>` sets it for a repository. A `clone` run through gitaway sets it in the new repository automatically.
4. The one profile whose `[hosts]` lists the remote's host. If several do, gitaway runs each one's home check and uses the one that passes. If none or more than one passes, it uses your `default_profile` if that's one of them (one server set up twice, say for ssh and https). Otherwise it stops and asks you to choose, with `gitaway use` in a repository or `--profile` for one command (such as a `clone`), because it never guesses.
5. `default_profile` in `config.toml`.

If none applies, the command goes to git unchanged.

The remotes are every server the command may contact: the remote named in the command (`git push upstream`), else the URL being cloned, else the remote git would use itself (the current branch's upstream, then `origin`). All of that remote's URLs count, since a push goes to every push URL, as do every remote for `fetch --all` and `remote update`, and the repository's submodules. If the chosen profile handles any of them, the whole command uses it.

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

If the server uses a preshared key (a `PresharedKey` line under `[Peer]`, which wg-easy and PiVPN add by default), `gitaway setup` and `gitaway key import` take it from the config and store it with the device key. Importing a bare key keeps the stored preshared key, and `key rotate` keeps it too: it belongs to the server's entry for this device, not to the key pair. `gitaway profile show` says when a profile has one.

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
