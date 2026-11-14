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

Each binary contains a full copy of the code, a few megabytes each. That's deliberate: `cargo install` can only copy binaries, not create links, so two binaries is the only way it can install both names on every platform. Release archives for Linux and macOS hold one binary, and the installer scripts add a `git-away` symlink (on Windows, a hard link); the program doesn't care which name started it.

## What happens to a command

```text
gitaway [--profile <name>] [git global options] <command> [args]
│
├─ nothing, -h, --help ───────────────▶ gitaway's short help
├─ --version ─────────────────────────▶ gitaway's version
├─ setup, profile, use, peer, key, ───▶ gitaway's own commands
│  check, tunnel
├─ network command ───────────────────▶ choose profile ─▶ home check ─▶ git
│  (clone, fetch, pull, push, ls-remote,                       │
│   remote update/show/prune, remote add -f,                   └─ not home ─▶ git through the tunnel
│   remote set-head -a, submodule update/add,
│   archive --remote)
└─ anything else ─────────────────────▶ git, exactly as typed
```

- gitaway's own commands take precedence over git aliases with the same name. A name planned for a later release can be claimed early in `RESERVED_COMMANDS` (`src/cli/mod.rs`), so an alias can't silently change meaning when that command arrives; none is reserved now.
- Classifying a command skips git's global options (`-C dir`, `-c key=value`, `--git-dir=…`) to find the subcommand. clap only ever sees gitaway's own commands, because it would reject git's options.
- For a network command, gitaway asks git which remote URL the command will use (`git remote get-url`, `git ls-remote --get-url`), then picks a profile using the order in the [configuration reference](configuration.md#which-profile-is-used).
- If the profile's `[hosts]` doesn't list the remote's host, git runs without a home check.
- If one of gitaway's own git lookups fails (a bad `-C` folder, a broken config file), the command goes to git as typed. The real command fails the same way before touching the network, so the user sees git's own message and exit code.
- At home, on Linux and macOS, gitaway replaces itself with git (`exec`), so exit codes, signals and terminal behaviour are exactly git's. On Windows it runs git and waits.
- Away, gitaway must stay alive to run the tunnel, so it runs git as a child and waits (`src/cli/away.rs`). Ctrl+C reaches git directly (it's in the same process group) and gitaway waits for it to finish; a SIGTERM sent only to gitaway is passed on to git. Then the proxy and tunnel close and gitaway exits with git's code. A per-profile file lock (`src/config/lock.rs`) makes a second command on the same profile wait, because a WireGuard server keeps one session per device key.
- A different host key at the home address means away, not an error: another network can have its own SSH server at that address. The tunnel is authenticated by WireGuard, so using it is safe, and a note says the key may have changed in case you are home.

## The home check

"Am I at home?" is answered by identity, not by address. An address match can be spoofed: a café network can use the same IP range as your home.

1. Connect to the profile's `[home].ssh` server, giving up after 200 ms.
2. Start an SSH key exchange, offering only the pinned key's type (a server with several host keys presents whichever type the client asks for). For RSA, only SHA-2 signatures are offered.
3. The server signs the exchange with its host key. The SSH library (`russh`) checks that signature before gitaway sees the key, so a server that only knows the public key can't pass.
4. Compare the key with the pinned `host_key`. The connection is closed without logging in.

The exchange is given 1 second. A match means home; anything else (a different key, a timeout, a refused connection, or no key pinned yet) means away. The code is in `src/home_check.rs`.

`gitaway setup` (`src/cli/setup_cmd.rs`) pins the key in the first place, while finding the git server through the tunnel (see [Setting up](#setting-up)). `gitaway check` (`src/cli/check_cmd.rs`) pins or re-checks it later. Both run the same exchange over a connection through the tunnel (`home_check::capture`), offering every type gitaway can pin, best first (Ed25519, ECDSA, RSA with SHA-2), or the already pinned type first when there is one, so the comparison is like for like. WireGuard has authenticated the far end, so there's no trust-on-first-use step (except when setup runs at home; see below). A wrong `[hosts]` mapping can only make gitaway pin a key that doesn't match at home, which means using the tunnel there too: it fails safe.

## Setting up

`gitaway setup` turns the config file a VPN server gives a device into a profile, so users never copy keys or addresses by hand:

```text
 VPN config ──client_config::read──▶ transport settings, device key, routes
 clone URL ─────────────────────────▶ the git server's home address and SSH port
 the git server answers on this network, and the user says they're home?
    yes ──▶ pin its key directly; the tunnel isn't tested
 otherwise: open the tunnel, try ClientConfig::likely_addresses in order
    an SSH server answers ──▶ pin the key it proves; [hosts] maps home address → that address
    no handshake, or nothing answers ──▶ error; nothing saved
```

The config tells gitaway everything about the tunnel but not where the git server is inside it. Its routes (WireGuard's `AllowedIPs`) narrow that down: a single routed address first, then the home address itself if the home network is routed, then the first address of the device's /24. Each VPN's format is a `ConfigFormat` next to its transport (see [adding a transport](adding-a-transport.md)), so setup itself doesn't change when a VPN is added.

Pinning at home is trust on first use: something answering at the home address on the local network is taken to be the git server once the user confirms they're home. It's offered because the tunnel often can't be reached from inside the home network, and `gitaway check` from another network later confirms the key through the tunnel.

## Away from home: the proxy

git's URLs are never rewritten. For one network command, gitaway starts a SOCKS5 proxy on `127.0.0.1` and points git at it through git's environment:

```text
 git ── https:// ─▶ http.proxy=socks5h://gitaway:<token>@127.0.0.1:<port>  ┐
     ── ssh://   ─▶ GIT_SSH_COMMAND="<your ssh> -o ProxyCommand=…"           ├─▶ proxy
                     (runs `gitaway __proxy-connect %h %p`)                   ┘     │
                                                                                   ▼
                       host:port in the profile's [hosts]? ── yes ─▶ Transport::connect(tunnel address)
                                                           └─ no ──▶ direct, as git would connect
```

- Because git still sees the real host name, TLS certificates, credential helpers and `known_hosts` keep working.
- Settings go in git's environment (`GIT_CONFIG_COUNT`), never its command line, which other users can read. Existing `GIT_CONFIG_COUNT` entries are kept.
- The user's own ssh command is kept (`GIT_SSH_COMMAND`, `core.sshCommand`, `GIT_SSH`, or `ssh`), with the `ProxyCommand` option added. Options on ssh's command line win over `~/.ssh/config`, so a `ProxyJump` there can't route around the tunnel. A test proves it. ssh keeps the first proxy it's given, so a `ProxyJump`, `-J` or `ProxyCommand` inside the user's own ssh command would win over gitaway's: away from home, with an ssh home URL, gitaway refuses and explains how to move it to `~/.ssh/config`.
- The helper receives `%h %p %n`: the host after `~/.ssh/config`, the port, and the name ssh was given. When an alias's `Hostname` differs from the alias, the helper first asks the proxy for the alias as a home-only request (SOCKS username `gitaway-home-only`: through the tunnel, or no connection at all), then for `%h` as usual. So a `[hosts]` key written as an alias still goes through the tunnel.
- git picks the most specific proxy setting (`remote.<name>.proxy`, then `http.<url>.proxy`, then `http.proxy`). For each home https URL, gitaway also sets the first two to its proxy, for this command only, so a setting of the user's can't send a home URL around the tunnel.
- The decision to use the proxy counts every URL the command may contact (all of a remote's push URLs, every remote for `fetch --all` and `remote update`, submodules). One home host among them is enough; the proxy still connects to the others directly.
- The proxy only listens on loopback and needs a random 256-bit token that lives for one command. git would normally offer that proxy password to your credential helper to store, so gitaway sets an empty helper scoped to the proxy's URL. A test proves git does try to store it, and another that it doesn't with gitaway's setting.
- `NO_PROXY` and `no_proxy` are removed from git's environment, so they can't send a home host around the proxy.
- `git://` isn't covered: git runs `core.gitProxy` as a bare program, with no way to pass the helper's arguments.

The code is in `src/proxy/`: `socks.rs` (the protocol), `mod.rs` (the server and routing), `helper.rs` (`__proxy-connect`) and `git_env.rs` (git's environment).

## Keeping the tunnel open: `gitaway tunnel`

`src/cli/tunnel_cmd.rs` serves the profile's `[web] url` on `localhost` through the tunnel, until Ctrl+C:

```text
browser ──▶ 127.0.0.1:<port> / [::1]:<port> ──▶ Transport::connect(web address through [hosts]) ──▶ git server
```

- It runs the home check first; at home it just prints the web address.
- It listens before taking the profile's lock or reading the device key, so a busy port is reported at once. It never picks another port by itself.
- The tunnel is opened as `Lifetime::LongRunning`, which turns on WireGuard keepalives, so a router doesn't forget a quiet tunnel. One connection is made before it says it's up, so a tunnel that can't work says why at once.
- It holds the profile's lock until it stops, so other gitaway commands for that profile wait.
- Each browser connection becomes its own connection through the tunnel, with bytes copied both ways. Nothing is changed, which is why `https://` works (with a certificate warning) and why the server still sees its own home address as its name.
- Before a plain-HTTP connection goes through, `src/cli/web_guard.rs` reads its first request head and checks that `Host` names `localhost`, `127.0.0.1` or `[::1]` with the tunnel's port. Otherwise it answers `421` and closes, so a web page using DNS rebinding can't reach the server. TLS connections pass unchecked: a page under another name fails the certificate check.

## Code map

| Path | Responsibility |
|---|---|
| `src/cli/` | The command line: dispatch (`mod.rs`), clap definitions (`args.rs`), prompts, and one module per command. `network.rs` decides what happens to a network command. |
| `src/git/` | Everything about git: classifying a command line, parsing remote URLs, reading a repository's remotes and config, and running git. |
| `src/config/` | Profile and config files: their format, validation, location, atomic writes, and which profile applies. |
| `src/home_check.rs` | The home check. |
| `src/hardening.rs` | Process hardening at start-up: on Linux, gitaway marks itself not dumpable before it loads a secret. |
| `src/keys/` | Device keys: generating them, and keeping them in the OS keychain or a private file. |
| `src/proxy/` | The per-command SOCKS5 proxy git talks to when away, and git's environment for it. |
| `src/transport/` | Reaching home from elsewhere: the `Transport` trait, the WireGuard transport, a userspace TCP/IP stack (`netstack.rs`), and readers for the client configs VPN servers hand out (`client_config.rs`), used by `gitaway setup`. See [adding a transport](adding-a-transport.md). |
| `src/address.rs` | The `host[:port]` syntax, shared by remote URLs and profiles. |
| `src/error.rs` | The top-level error type and exit codes. |
| `xtask/` | Maintenance tasks (`cargo xtask …`), such as generating the man pages. Not part of the published crate. |
| `tests/cli.rs` | End-to-end tests of the real binary, with a fake `git` and a test SSH server. |
| `tests/proxy.rs` | Real git and ssh through the proxy and the real helper binary. |
| `examples/home/` | A WireGuard server and Forgejo in Docker, and `e2e.sh`, which runs the real binary against them as a user would. Runs in CI. |

Each module starts with a `//!` comment that explains its purpose, how it fits in and how to extend it. `cargo doc --open` shows them all.

## Man pages

`cargo xtask man` generates the pages into `target/man/` (or `--out <dir>`). Read one with `man target/man/gitaway.1`.

- `gitaway.1` is written by hand in `xtask/src/man.rs`, because gitaway's top level isn't a clap command. A test fails if a gitaway command is missing from its command list.
- `git-away.1` is the same page under the name git opens for `git away --help`.
- Every gitaway command (`gitaway-profile-add.1`, …) gets a page generated from its clap definition, so its flags and help text can't drift from the code.

**Man pages are installed by packages, not by `cargo install`.** `man` only reads files from man directories; it can't ask a program for its page. `cargo install` can only copy binaries, so the release workflow generates the pages at build time (`cargo xtask man --out dist-man`, run by `.github/workflows/release-build-setup.yml`) and puts them in every archive. The installer scripts don't place them; the Homebrew formula (in the separate `nikkel/homebrew-tap` repo, which follows our releases) does. The pages aren't committed, so there's no copy to keep in sync. CI generates them on every push, so a broken page is caught early. Users of `cargo install` use `--help` instead.

## Supported git versions

The minimum is git 2.34, the version in Ubuntu 22.04. CI runs the tests against it, against the latest git, and against Apple's git on macOS. Use only git commands and options that 2.34 supports.

## Extending

- **A gitaway command:** see the `# Extending` section at the top of `src/cli/mod.rs`.
- **A profile setting:** see `src/config/profile.rs`, and `src/cli/profile_cmd.rs` if `profile add` should ask for it. Update the [configuration reference](configuration.md) in the same change.
- **A transport (another kind of VPN):** see [adding a transport](adding-a-transport.md).
- **A network command:** add it to `NETWORK_COMMANDS` or `NETWORK_SUBCOMMANDS` in `src/git/classify.rs`, with a test.
