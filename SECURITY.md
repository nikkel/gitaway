# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's [private vulnerability reporting](https://github.com/nikkel/gitaway/security/advisories/new), not in a public issue.

Include what you found, how to reproduce it and the impact you expect. You'll get an acknowledgement, and a fix or a plan will be agreed with you before anything is disclosed.

## Supported versions

gitaway has only had a pre-release (0.1.0-alpha.1), for testing. Pre-releases aren't supported: security fixes go into the next release. From 0.1.0 on, security fixes go into the latest release.

## Scope

gitaway handles VPN private keys, verifies the identity of your home server, and listens on loopback ports. Issues in any of these are in scope, such as a way to make the home check pass for the wrong server, leak a key, or reach a listener from another machine.
