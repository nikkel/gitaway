//! Keeps web pages from using `gitaway tunnel`'s local port under their own
//! name ("DNS rebinding").
//!
//! A malicious site can make your browser look its own name up as
//! 127.0.0.1, then load `http://evil.example:<port>/` from the tunnel's port.
//! The browser thinks that's the site's own server, so the page could read
//! whatever your git server shows a visitor who isn't signed in. Your cookies
//! aren't sent (they belong to `localhost`), but that can still be public
//! repositories, the user list, or a sign-up form.
//!
//! The browser always says which name it's visiting, in the request's `Host`
//! header. So before a plain-HTTP connection goes through the tunnel, its
//! first request must name this computer: `localhost`, `127.0.0.1` or
//! `[::1]`, with the tunnel's port. Anything else is answered with
//! `421 Misdirected Request` and closed.
//!
//! ```text
//!  first byte 0x16 (TLS) ──────────────────────▶ forward: a page using another
//!                                                name fails the certificate check
//!  request head (up to 16 KiB, within 10 s)
//!    Host is localhost / 127.0.0.1 / [::1]:port ▶ forward, exactly as received
//!    anything else ───────────────────────────▶ 421, close; the tunnel isn't used
//! ```
//!
//! Only each connection's first request is checked: browsers keep separate
//! connections for each site, so a page under another name never gets a
//! connection that started with a good request. Programs on this computer
//! could get past it, but they can reach the port anyway (see the README's
//! security model). Nothing is ever changed: the bytes read here are passed
//! on as they are.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// The most of a request head that's read to find its `Host`. Browsers send
/// a few KiB at most.
const MAX_HEAD: usize = 16 * 1024;

/// How long a new connection gets to send its first request.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The first byte of a TLS handshake.
const TLS_HANDSHAKE: u8 = 0x16;

/// What to do with a new connection.
#[derive(Debug, PartialEq, Eq)]
pub enum Opening {
    /// Forward it, starting with these bytes, already read.
    Forward(Vec<u8>),
    /// Answer with [`refusal`] and close it: a request for another name.
    Refuse,
    /// Close it: nothing usable arrived in time, or the request head was too
    /// long.
    Drop,
}

/// Reads the start of a new connection to the tunnel's local port `port`
/// and decides what to do with it.
pub async fn read_opening<S: AsyncRead + Unpin>(stream: &mut S, port: u16) -> Opening {
    tokio::time::timeout(HEAD_TIMEOUT, read_head(stream, port))
        .await
        .unwrap_or(Opening::Drop)
}

async fn read_head<S: AsyncRead + Unpin>(stream: &mut S, port: u16) -> Opening {
    let mut head = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return Opening::Drop,
            Ok(read) => read,
        };
        head.extend_from_slice(&buffer[..read]);
        if head[0] == TLS_HANDSHAKE {
            return Opening::Forward(head);
        }
        if let Some(end) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            return if names_this_computer(&head[..end], port) {
                Opening::Forward(head)
            } else {
                Opening::Refuse
            };
        }
        if head.len() > MAX_HEAD {
            return Opening::Drop;
        }
    }
}

/// Whether a request head (without the blank line ending it) asks for this
/// computer on `port`: a path, not a full URL, and exactly one `Host` header
/// naming `localhost`, `127.0.0.1` or `[::1]` with that port (or with no
/// port, for port 80).
pub fn names_this_computer(head: &[u8], port: u16) -> bool {
    let Ok(head) = std::str::from_utf8(head) else {
        return false;
    };
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    // `GET http://evil.example/ HTTP/1.1` makes servers ignore `Host`.
    let target = request_line.split(' ').nth(1).unwrap_or_default();
    if !target.starts_with('/') {
        return false;
    }
    let mut hosts = lines.filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("host")
            .then(|| value.trim().to_ascii_lowercase())
    });
    let (Some(host), None) = (hosts.next(), hosts.next()) else {
        return false;
    };
    let names = ["localhost", "127.0.0.1", "[::1]"];
    names
        .iter()
        .any(|name| host == format!("{name}:{port}") || (port == 80 && host == *name))
}

/// The answer to a refused request.
pub fn refusal(port: u16) -> String {
    let body = format!(
        "gitaway: this tunnel only answers requests for localhost:{port}. Open \
         http://localhost:{port}/ instead.\n"
    );
    format!(
        "HTTP/1.1 421 Misdirected Request\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(host_lines: &str) -> String {
        format!("GET /alice/app HTTP/1.1\r\n{host_lines}User-Agent: test")
    }

    #[test]
    fn requests_naming_this_computer_and_port_are_allowed() {
        for host in [
            "localhost:3000",
            "127.0.0.1:3000",
            "[::1]:3000",
            "LocalHost:3000",
        ] {
            assert!(
                names_this_computer(head(&format!("Host: {host}\r\n")).as_bytes(), 3000),
                "{host}"
            );
        }
        // Header names aren't case-sensitive.
        assert!(names_this_computer(
            head("host: localhost:3000\r\n").as_bytes(),
            3000
        ));
        // Port 80 is left out by browsers.
        assert!(names_this_computer(
            head("Host: localhost\r\n").as_bytes(),
            80
        ));
    }

    #[test]
    fn other_names_ports_and_odd_requests_are_refused() {
        let cases = [
            ("another site", head("Host: evil.example:3000\r\n")),
            ("another port", head("Host: localhost:8080\r\n")),
            ("no port", head("Host: localhost\r\n")),
            ("the home address", head("Host: 192.0.2.10:3000\r\n")),
            ("no Host", head("")),
            (
                "two Hosts",
                head("Host: localhost:3000\r\nHost: evil.example:3000\r\n"),
            ),
            (
                "a full URL",
                "GET http://evil.example/ HTTP/1.1\r\nHost: localhost:3000".to_owned(),
            ),
            ("not HTTP", "hello".to_owned()),
        ];
        for (what, head) in cases {
            assert!(!names_this_computer(head.as_bytes(), 3000), "{what}");
        }
        assert!(!names_this_computer(&[0xff, 0xfe, b'\r', b'\n'], 3000));
    }

    fn opening(bytes: &[u8]) -> Opening {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut stream = bytes;
            read_opening(&mut stream, 3000).await
        })
    }

    #[test]
    fn the_opening_decides_and_keeps_every_byte_read() {
        let good = b"GET / HTTP/1.1\r\nHost: localhost:3000\r\n\r\nbody";
        assert_eq!(opening(good), Opening::Forward(good.to_vec()));
        let tls = [TLS_HANDSHAKE, 3, 1, 0, 5];
        assert_eq!(opening(&tls), Opening::Forward(tls.to_vec()));
        let rebound = b"GET / HTTP/1.1\r\nHost: evil.example:3000\r\n\r\n";
        assert_eq!(opening(rebound), Opening::Refuse);
        // Closed before a whole head arrived, or a head that never ends.
        assert_eq!(opening(b"GET / HTTP/1.1\r\nHost: local"), Opening::Drop);
        assert_eq!(opening(&vec![b'a'; MAX_HEAD + 4096]), Opening::Drop);
    }

    #[test]
    fn the_refusal_is_a_complete_http_response() {
        let refusal = refusal(3000);
        let (head, body) = refusal.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 421 "));
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
        assert!(body.contains("localhost:3000"));
    }
}
