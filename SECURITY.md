# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's [private vulnerability reporting](https://github.com/nikkel/gitaway/security/advisories/new), not in a public issue.

Include what you found, how to reproduce it and the impact you expect. You'll get an acknowledgement, and a fix or a plan will be agreed with you before anything is disclosed.

## Supported versions

Security fixes go into the latest release, starting with 0.1.0. Older releases, and the 0.1.0-alpha.1 pre-release, aren't updated: update to the latest.

## Release binaries

From 0.1.1, releases include prebuilt binaries made by a GitHub Actions workflow in this repository, from the tagged commit only (pull requests don't build release files). Each archive of binaries has a SHA-256 checksum and a GitHub artifact attestation, and each release is immutable. The installers, `source.tar.gz` and `sha256.sum` have no attestation: `gh release verify-asset` checks that they belong to the release. How to check them is in the [README](README.md#verifying-releases); always pass `--source-ref refs/tags/<tag>` to `gh attestation verify`. The shell installer checks the checksum itself if the `sha256sum` command is available, and otherwise says it's skipping the check; the PowerShell installer never checks. macOS binaries in releases after 0.2.0 are signed with a Developer ID certificate and notarized by Apple. Earlier ones aren't, so macOS may refuse one downloaded in a browser until you approve it. Windows binaries aren't signed. A concern about a release file, or about the workflow, is in scope: report it privately, as above.

## Scope

gitaway handles VPN private keys, verifies the identity of your home server, and listens on loopback ports. Issues in any of these are in scope, such as a way to make the home check pass for the wrong server, leak a key, or reach a listener from another machine. `gitaway tunnel`'s local port is meant to be reachable by every program and user on the same computer while it runs (see the README's security model); reaching it from another machine, or through it anything but the profile's web address, is in scope. So is using the proxy it runs for other gitaway commands without the token in its runs file, or reaching through it anything but the profile's `[hosts]`.
