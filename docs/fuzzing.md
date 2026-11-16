# Fuzzing

A fuzzer feeds a parser millions of generated inputs, mutating the ones that
reach new code, and stops at the first panic, hang or runaway memory use. It
finds the inputs nobody thinks to write a test for. gitaway's targets are in
[`fuzz/`](../fuzz/), one per parser that reads input from outside the program.

| Target | Reads | Also checks |
|---|---|---|
| `socks_accept` | the SOCKS5 handshake any local program can send the proxy | it never accepts a client that didn't send the token |
| `web_head` | the start of a connection to `gitaway tunnel`'s web port | it forwards exactly the bytes it read |
| `client_config` | a VPN client config given to `gitaway setup` | |
| `profile` | profile and global config files, and their error messages | |
| `values` | single values: host and port, host key, public key, address, web URL, profile name | |
| `remote_url` | git remote URLs | |
| `git_args` | the arguments of a git command | |

## Running

Fuzzing needs nightly Rust and [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz),
on Linux or macOS:

```console
$ rustup toolchain install nightly
$ cargo +nightly install cargo-fuzz
$ cd fuzz
$ cargo +nightly fuzz run socks_accept -- -max_total_time=600
```

`fuzz/` is its own workspace: `cargo build` and `cargo test` at the top
never build it, and it isn't in the published crate. CI only checks that it
builds. Its `Cargo.lock` starts as a copy of the main one, so it fuzzes the
same dependency versions; after updating the main one, copy it over again and
run `cargo check --manifest-path fuzz/Cargo.toml`. Without rustup (Rust
from a package manager), run it in a container instead:

```console
$ docker run --rm -it -v "$PWD":/repo -w /repo/fuzz rust:1 \
    sh -c 'rustup toolchain install nightly --profile minimal &&
           cargo +nightly install cargo-fuzz &&
           cargo +nightly fuzz run socks_accept -- -max_total_time=600'
```

Inputs that reach new code are kept in `fuzz/corpus/<target>/` and reused by
the next run. Neither the corpus nor `fuzz/artifacts/` is committed.

## When it finds something

It stops and saves the input as `fuzz/artifacts/<target>/crash-…` (or
`timeout-…`, `oom-…`). Then:

1. Run that input again to see the failure:
   `cargo +nightly fuzz run <target> artifacts/<target>/<file>`, from
   `fuzz/`.
2. Write an ordinary unit test with that input, next to the code, and check
   it fails.
3. Fix the code; the test then passes, and keeps the bug from coming back
   without anyone needing to fuzz.

## Adding a target

Add `fuzz/fuzz_targets/<name>.rs` (start from a small one, such as
`remote_url.rs`) and a `[[bin]]` entry for it in `fuzz/Cargo.toml`. A
target that can check more than "doesn't panic" should: say what must
always hold, as `socks_accept` does with the token.
