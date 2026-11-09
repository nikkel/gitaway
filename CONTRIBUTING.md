# Contributing

Thanks for helping. This guide covers building, testing and the conventions the project follows.

## Build and test

You need:

- Rust 1.95 or newer;
- `git` 2.34 or newer on your `PATH`;
- OpenSSH's `ssh-keygen`, which a few tests use to check gitaway against the keys and fingerprints OpenSSH produces. It's installed on most systems; on minimal Debian or Ubuntu, install `openssh-client`.

```console
$ cargo build
$ cargo test
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo fmt --all
$ cargo xtask man
```

CI runs all of these on Linux and macOS (Windows is best-effort). It also runs the tests against git 2.34 and Apple's git, runs an end-to-end test against a real Forgejo behind a real WireGuard server, and scans for secrets. A pull request should pass them before review.

The end-to-end test needs Docker with Compose. Run it with `examples/home/e2e.sh` after `cargo build`; [`examples/home/`](examples/home/README.md) explains it.

## Where things live

The crate is a library with two thin binaries. Each module starts with a `//!` comment explaining its purpose, how it fits in and how to extend it. Every public item has `///` docs (the `missing_docs` lint enforces this). Contributor guides live in [`docs/`](docs/), starting with [`docs/architecture.md`](docs/architecture.md), and are updated in the same change as the code they describe. User-facing settings are documented in [`docs/configuration.md`](docs/configuration.md).

## Man pages

`cargo xtask man` generates the man pages into `target/man/`, where you can read them with `man target/man/gitaway.1`. They aren't committed: package builds generate them. A new gitaway command needs a line in `COMMANDS` in `xtask/src/man.rs`, and a test fails until it has one. See [`docs/architecture.md`](docs/architecture.md#man-pages).

## Tests

- Test behaviour through public interfaces, including failures and edge cases.
- Every test must be able to fail for a real defect. Don't assert on exact wording unless it's the contract.
- Mock only what's outside the system or slow. Avoid sleeps, shared state and dependence on test order.
- A bug fix comes with a test that fails without the fix.

## Example values only

This repository is public. In code, tests, docs and examples, use only documentation values:

- IPs from `192.0.2.0/24`, `198.51.100.0/24` or `203.0.113.0/24` (RFC 5737), and `10.0.0.0/8` for tunnel addresses.
- Hostnames under `example.com` or `example.net`.
- Usernames like `alice`.

Never paste real hostnames, IPs, ports, keys, fingerprints or usernames from your own setup.

## Commits

Use `<type>: <short description>`, lower case and imperative, with one logical change per commit. Types: `feature`, `update`, `fix`, `docs`, `refactor`, `test`, `chore`. Add a body when the reason isn't obvious. Update [`CHANGELOG.md`](CHANGELOG.md) for changes that affect users.

## Licence

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 licence, shall be dual licensed under MIT OR Apache-2.0, without any additional terms or conditions.
