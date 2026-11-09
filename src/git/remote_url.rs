//! Parse git remote URLs into a scheme, host and port.
//!
//! gitaway needs the host of a remote to match it against a profile's
//! `[hosts]` table. git accepts several URL forms, and the `url` crate doesn't
//! understand the scp-like one, so this module handles them all:
//!
//! | Form | Example |
//! |---|---|
//! | URL | `ssh://git@192.0.2.10:2222/alice/app.git`, `https://example.com/app.git` |
//! | scp-like | `git@192.0.2.10:alice/app.git`, `[git@example.com:2222]:app.git` |
//!
//! Local paths, `file://` URLs and other transports (`ext::`, remote helpers)
//! return `None`, because they never go over the network gitaway manages.
//!
//! Usernames and passwords in a URL are dropped while parsing, so a
//! [`RemoteUrl`] can be logged or printed without leaking credentials.
//!
//! # Extending
//!
//! A new network scheme goes in [`Scheme`] and in `scheme_from_name`, with its
//! default port in [`Scheme::default_port`].

use std::net::Ipv6Addr;

use crate::address::HostPort;

/// The transport a remote URL uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Scheme {
    /// `ssh://`, `git+ssh://`, `ssh+git://` or the scp-like form.
    Ssh,
    /// `git://`, git's own unauthenticated protocol.
    Git,
    /// `http://`.
    Http,
    /// `https://`.
    Https,
}

impl Scheme {
    /// The port used when the URL doesn't name one.
    ///
    /// ```
    /// use gitaway::git::remote_url::Scheme;
    /// assert_eq!(Scheme::Ssh.default_port(), 22);
    /// ```
    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Ssh => 22,
            Scheme::Git => 9418,
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }
}

/// The network location of a git remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    /// How git connects.
    pub scheme: Scheme,
    /// The host, lower-cased. IPv6 addresses are stored without brackets.
    pub host: String,
    /// The port, if the URL names one.
    pub port: Option<u16>,
}

impl RemoteUrl {
    /// The port git will connect to: the explicit one, or the scheme's default.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(self.scheme.default_port())
    }
}

/// Parses a git remote URL. Returns `None` for local paths, unsupported
/// transports and malformed URLs.
///
/// # Examples
///
/// ```
/// use gitaway::git::remote_url::{parse, Scheme};
///
/// let url = parse("ssh://git@192.0.2.10:2222/alice/app.git").unwrap();
/// assert_eq!(url.scheme, Scheme::Ssh);
/// assert_eq!(url.host, "192.0.2.10");
/// assert_eq!(url.port, Some(2222));
///
/// let url = parse("git@example.com:alice/app.git").unwrap();
/// assert_eq!(url.host, "example.com");
/// assert_eq!(url.effective_port(), 22);
///
/// assert!(parse("/srv/git/app.git").is_none());
/// ```
pub fn parse(url: &str) -> Option<RemoteUrl> {
    match url.split_once("://") {
        Some((scheme, rest)) => parse_url_form(scheme, rest),
        None => parse_scp_form(url),
    }
}

/// `scheme://[user[:password]@]host[:port][/path]`
fn parse_url_form(scheme: &str, rest: &str) -> Option<RemoteUrl> {
    let scheme = scheme_from_name(scheme)?;
    let authority = rest.split('/').next().unwrap_or_default();
    let HostPort { host, port } = strip_userinfo(authority).parse().ok()?;
    Some(RemoteUrl { scheme, host, port })
}

/// `[user@]host:path` or `[user@][host:port]:path`.
///
/// Like git, this is only the scp-like form if a colon comes before any
/// slash. Otherwise it's a local path.
fn parse_scp_form(url: &str) -> Option<RemoteUrl> {
    if is_windows_drive_path(url) {
        return None;
    }
    let host_end = scp_host_end(url)?;
    if url[host_end..].starts_with("::") {
        // `<transport>::<address>`: a git remote helper such as `ext::`.
        return None;
    }
    let authority = strip_userinfo_outside_brackets(&url[..host_end]);

    let address: HostPort = match authority.strip_prefix('[') {
        Some(bracketed) => {
            // The user may also be inside the brackets: `[git@host:22]:path`.
            let inner = strip_userinfo(bracketed.strip_suffix(']')?);
            if inner.parse::<Ipv6Addr>().is_ok() {
                format!("[{inner}]").parse().ok()?
            } else {
                inner.parse().ok()?
            }
        }
        // The host ends at the first colon, so there's no port here.
        None => authority.parse().ok()?,
    };
    let HostPort { host, port } = address;

    Some(RemoteUrl {
        scheme: Scheme::Ssh,
        host,
        port,
    })
}

/// The index of the colon that ends the host in the scp-like form, or `None`
/// if this is a local path.
fn scp_host_end(url: &str) -> Option<usize> {
    // A bracketed host may contain colons, so look for the colon after `]`.
    // Brackets only count at the start or straight after `user@`, so a `[`
    // later in the path (`host:dir[1]`) is just part of the path.
    let search_from = match bracket_start(url) {
        Some(open) => url[open..].find(']')? + open,
        None => 0,
    };
    let colon = url[search_from..].find(':')? + search_from;
    if url[..colon].contains('/') {
        return None;
    }
    Some(colon)
}

/// Where a bracketed host starts in the scp-like form, if there is one.
fn bracket_start(url: &str) -> Option<usize> {
    if url.starts_with('[') {
        return Some(0);
    }
    let at = url.find('@')?;
    let user = &url[..at];
    let is_user = !user.contains([':', '/', '[']);
    (is_user && url[at + 1..].starts_with('[')).then_some(at + 1)
}

/// Drops `user@` before a bracketed host (`git@[host:22]`), or from a plain
/// `user@host`. An `@` inside brackets is left for the caller.
fn strip_userinfo_outside_brackets(authority: &str) -> &str {
    match bracket_start(authority) {
        Some(open) => &authority[open..],
        None => strip_userinfo(authority),
    }
}

fn scheme_from_name(name: &str) -> Option<Scheme> {
    match name.to_ascii_lowercase().as_str() {
        "ssh" | "git+ssh" | "ssh+git" => Some(Scheme::Ssh),
        "git" => Some(Scheme::Git),
        "http" => Some(Scheme::Http),
        "https" => Some(Scheme::Https),
        _ => None,
    }
}

/// Drops `user@` or `user:password@`. The last `@` wins, as in git.
fn strip_userinfo(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

/// `C:\repo` or `C:/repo`. git treats these as local paths on Windows.
fn is_windows_drive_path(url: &str) -> bool {
    let bytes = url.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(scheme: Scheme, host: &str, port: Option<u16>) -> Option<RemoteUrl> {
        Some(RemoteUrl {
            scheme,
            host: host.to_owned(),
            port,
        })
    }

    #[test]
    fn url_forms() {
        let cases = [
            (
                "ssh://192.0.2.10/app.git",
                remote(Scheme::Ssh, "192.0.2.10", None),
            ),
            (
                "ssh://git@192.0.2.10:2222/a/app.git",
                remote(Scheme::Ssh, "192.0.2.10", Some(2222)),
            ),
            (
                "git+ssh://example.com/app",
                remote(Scheme::Ssh, "example.com", None),
            ),
            (
                "ssh+git://example.com/app",
                remote(Scheme::Ssh, "example.com", None),
            ),
            (
                "git://example.com:9419/app",
                remote(Scheme::Git, "example.com", Some(9419)),
            ),
            (
                "http://192.0.2.10:3000/a/app.git",
                remote(Scheme::Http, "192.0.2.10", Some(3000)),
            ),
            (
                "https://example.com/a/app.git",
                remote(Scheme::Https, "example.com", None),
            ),
            (
                "https://example.com",
                remote(Scheme::Https, "example.com", None),
            ),
            (
                "HTTPS://Example.COM/app",
                remote(Scheme::Https, "example.com", None),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input), expected, "{input}");
        }
    }

    #[test]
    fn scp_like_forms() {
        let cases = [
            (
                "git@192.0.2.10:alice/app.git",
                remote(Scheme::Ssh, "192.0.2.10", None),
            ),
            (
                "example.com:app.git",
                remote(Scheme::Ssh, "example.com", None),
            ),
            ("git@nas:app.git", remote(Scheme::Ssh, "nas", None)),
            (
                "[git@example.com:2222]:app.git",
                remote(Scheme::Ssh, "example.com", Some(2222)),
            ),
            (
                "git@[example.com:2222]:app.git",
                remote(Scheme::Ssh, "example.com", Some(2222)),
            ),
            (
                "[2001:db8::1]:app.git",
                remote(Scheme::Ssh, "2001:db8::1", None),
            ),
            (
                "git@example.com:/srv/app.git",
                remote(Scheme::Ssh, "example.com", None),
            ),
            (
                "git@example.com:dir[1]/app.git",
                remote(Scheme::Ssh, "example.com", None),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input), expected, "{input}");
        }
    }

    #[test]
    fn ipv6_in_url_form() {
        assert_eq!(
            parse("ssh://git@[2001:DB8::1]:2222/app.git"),
            remote(Scheme::Ssh, "2001:db8::1", Some(2222))
        );
        assert_eq!(
            parse("https://[2001:db8::1]/app.git"),
            remote(Scheme::Https, "2001:db8::1", None)
        );
    }

    #[test]
    fn credentials_are_not_kept() {
        let url = parse("https://alice:hunter2@example.com/app.git").unwrap();
        assert_eq!(url.host, "example.com");
        assert!(!format!("{url:?}").contains("hunter2"));
    }

    #[test]
    fn local_paths_and_other_transports_are_none() {
        for input in [
            "/srv/git/app.git",
            "./app",
            "../app.git",
            "app",
            "dir/with:colon",
            "file:///srv/git/app.git",
            "ext::ssh -i key example.com %S app",
            "s3://bucket/app",
            "C:\\repos\\app",
            "C:/repos/app",
            "",
        ] {
            assert_eq!(parse(input), None, "{input}");
        }
    }

    #[test]
    fn malformed_hosts_and_ports_are_none() {
        for input in [
            "ssh://:2222/app",
            "ssh://example.com:/app",
            "ssh://example.com:0/app",
            "ssh://example.com:65536/app",
            "ssh://example.com:22x/app",
            "ssh://[not-ipv6]/app",
            "ssh://[2001:db8::1]x/app",
            "http.proxy=http://example.com",
            ":app.git",
            "-oProxyCommand=evil:app",
            "[example.com:app.git",
        ] {
            assert_eq!(parse(input), None, "{input}");
        }
    }

    #[test]
    fn effective_port_uses_scheme_default() {
        assert_eq!(parse("git@example.com:app").unwrap().effective_port(), 22);
        assert_eq!(
            parse("git://example.com/app").unwrap().effective_port(),
            9418
        );
        assert_eq!(
            parse("http://example.com/app").unwrap().effective_port(),
            80
        );
        assert_eq!(
            parse("https://example.com/app").unwrap().effective_port(),
            443
        );
        assert_eq!(
            parse("https://example.com:8443/a")
                .unwrap()
                .effective_port(),
            8443
        );
    }
}
