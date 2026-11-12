# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's [private vulnerability reporting](https://github.com/nikkel/gitaway/security/advisories/new), not in a public issue.

Include what you found, how to reproduce it and the impact you expect. You'll get an acknowledgement, and a fix or a plan will be agreed with you before anything is disclosed.

## Supported versions

Security fixes go into the latest release, starting with 0.1.0. Older releases, and the 0.1.0-alpha.1 pre-release, aren't updated: update to the latest.

## Release binaries

From 0.1.1, releases include prebuilt binaries made by a GitHub Actions workflow in this repository, from the tagged commit only (pull requests don't build release files). Each file has a SHA-256 checksum and a GitHub artifact attestation, and each release is immutable. How to check them is in the [README](README.md#verifying-releases); always pass `--source-ref refs/tags/<tag>` to `gh attestation verify`. The shell installer checks the checksum itself; the PowerShell installer doesn't. macOS binaries aren't yet signed by Apple, so macOS may refuse a file downloaded in a browser until you approve it, and asks for your Keychain password again after each update (see the README). A concern about a release file, or about the workflow, is in scope: report it privately, as above.

## Scope

gitaway handles VPN private keys, verifies the identity of your home server, and listens on loopback ports. Issues in any of these are in scope, such as a way to make the home check pass for the wrong server, leak a key, or reach a listener from another machine.
