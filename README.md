# gitaway

[![CI](https://github.com/nikkel/gitaway/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/nikkel/gitaway/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/gitaway.svg)](https://crates.io/crates/gitaway)
[![docs.rs](https://img.shields.io/docsrs/gitaway)](https://docs.rs/gitaway)
[![MSRV](https://img.shields.io/crates/msrv/gitaway)](https://crates.io/crates/gitaway)
[![License](https://img.shields.io/github/license/nikkel/gitaway)](#license)

Reach your self-hosted git server from anywhere. At home, `gitaway` connects directly. Away from home, it opens a VPN tunnel that only gitaway itself uses: no system-wide VPN, no virtual network adapter, no root.

> **Status:** in planning. Nothing is usable yet.

## Verifying releases

There are no releases yet. When there are, each one will be an [immutable GitHub release](https://docs.github.com/en/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases): once it's published, its tag and binaries can't be changed or replaced, not even by the maintainer. GitHub signs an attestation of each release's contents, which you can check with the [GitHub CLI](https://cli.github.com/):

```console
$ gh release verify v0.1.0 --repo nikkel/gitaway
$ gh release verify-asset v0.1.0 ./gitaway-x86_64-unknown-linux-gnu.tar.xz --repo nikkel/gitaway
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
