//! Watching a long-running tunnel's contact with its VPN server, for
//! `gitaway tunnel`.
//!
//! ```text
//!  every 30 s: how long since the last handshake? (Transport::last_handshake)
//!    none for over 4 minutes ──▶ say "lost contact" once
//!                                every 30 s look for the server again: look its
//!                                name up, start a handshake
//!                                (Transport::find_server_again); say if it moved
//!    a new handshake ──────────▶ say "back in contact"
//! ```
//!
//! Why 4 minutes: WireGuard's keepalives don't count as traffic, so a quiet
//! tunnel doesn't renew its session every 2 minutes as a busy one does. Its
//! session expires after 3 minutes, and the next keepalive (within 25
//! seconds) starts a new handshake. So a healthy quiet tunnel can go about
//! 3½ minutes between handshakes.
//!
//! While the server is quiet, the tunnel's keepalives start new handshakes,
//! but only for 90 seconds: then gotatun stops trying until something is
//! sent. So once contact is lost, the watchdog starts a handshake every 30
//! seconds itself, pointing the tunnel at a new address first if the
//! server's name now gives one: after a router restart, say, when a dynamic
//! DNS name follows the home connection's new public address.

use std::time::Duration;

use tokio::time::Instant;

use crate::transport::Transport;

/// How often the tunnel is checked.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(30);

/// How long without a handshake counts as lost contact.
pub(crate) const LOST_AFTER: Duration = Duration::from_secs(4 * 60);

/// A change worth telling the user about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// No handshake for [`LOST_AFTER`].
    Lost,
    /// A handshake again, after [`Change::Lost`].
    Back,
}

/// What's known about the tunnel's contact with its server.
#[derive(Debug)]
pub(crate) struct Contact {
    last_handshake_at: Instant,
    lost: bool,
}

impl Contact {
    /// Starts watching a tunnel that has just proved it works, at `now`.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_handshake_at: now,
            lost: false,
        }
    }

    /// Whether contact is lost, as of the last [`Contact::observe`].
    pub(crate) fn is_lost(&self) -> bool {
        self.lost
    }

    /// Takes in what the tunnel said at `now`: how long ago its last
    /// handshake was, or `None` if it has no session (between sessions, or
    /// none since contact was lost). Returns the change, if there is one.
    pub(crate) fn observe(
        &mut self,
        now: Instant,
        since_handshake: Option<Duration>,
    ) -> Option<Change> {
        if let Some(handshake_at) = since_handshake.and_then(|age| now.checked_sub(age)) {
            self.last_handshake_at = self.last_handshake_at.max(handshake_at);
        }
        let silent = now.saturating_duration_since(self.last_handshake_at) > LOST_AFTER;
        match (self.lost, silent) {
            (false, true) => {
                self.lost = true;
                Some(Change::Lost)
            }
            (true, false) => {
                self.lost = false;
                Some(Change::Back)
            }
            _ => None,
        }
    }
}

/// Watches `transport` until the task is stopped, passing each message for
/// the user to `say`.
pub(crate) async fn watch(transport: &dyn Transport, mut say: impl FnMut(String)) {
    let mut contact = Contact::new(Instant::now());
    let mut checks = tokio::time::interval_at(Instant::now() + CHECK_EVERY, CHECK_EVERY);
    // After the computer sleeps, check once on waking rather than catching up.
    checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut lookup_failed = false;
    loop {
        checks.tick().await;
        let since_handshake = transport.last_handshake().await;
        match contact.observe(Instant::now(), since_handshake) {
            Some(Change::Lost) => {
                lookup_failed = false;
                say(format!(
                    "lost contact with the VPN server (no handshake for over {} minutes); \
                     still trying…",
                    LOST_AFTER.as_secs() / 60
                ));
            }
            Some(Change::Back) => say("back in contact with the VPN server.".to_owned()),
            None => {}
        }
        if !contact.is_lost() {
            continue;
        }
        match transport.find_server_again().await {
            Ok(Some(address)) => say(format!(
                "the VPN server's name now points to {address}; trying there."
            )),
            Ok(None) => {}
            // Expected while this computer is offline: say it once, not
            // every 30 seconds.
            Err(e) if !lookup_failed => {
                lookup_failed = true;
                say(format!("{e}"));
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{ConnectFuture, FindServerFuture, HandshakeFuture, TransportError};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_quiet_but_healthy_tunnel_is_never_called_lost() {
        let start = Instant::now();
        let mut contact = Contact::new(start);
        // Each session expires after 180 s, and the next keepalive starts a
        // new one up to 25 s later: a handshake every 205 s at worst, with no
        // session for the last 25 s of each. Checks don't line up with them.
        let gap = 205;
        for second in (20..2000).step_by(30) {
            let age = Duration::from_secs(second % gap);
            let reported = (age < Duration::from_secs(180)).then_some(age);
            let now = start + SECOND * u32::try_from(second).unwrap();
            assert_eq!(contact.observe(now, reported), None, "at {second} s");
        }
    }

    #[test]
    fn silence_is_lost_once_and_a_handshake_brings_it_back_once() {
        let start = Instant::now();
        let mut contact = Contact::new(start);
        assert_eq!(
            contact.observe(start + SECOND * 60, Some(SECOND * 60)),
            None
        );
        assert_eq!(contact.observe(start + SECOND * 240, None), None);
        assert_eq!(
            contact.observe(start + SECOND * 241, None),
            Some(Change::Lost)
        );
        assert!(contact.is_lost());
        assert_eq!(contact.observe(start + SECOND * 600, None), None);
        assert_eq!(
            contact.observe(start + SECOND * 630, Some(SECOND * 2)),
            Some(Change::Back)
        );
        assert!(!contact.is_lost());
        assert_eq!(
            contact.observe(start + SECOND * 660, Some(SECOND * 32)),
            None
        );
    }

    /// A tunnel whose server never answers, and whose name moves once.
    struct Silent {
        lookups: Mutex<u32>,
    }

    impl Transport for Silent {
        fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
            Box::pin(async move { Err(TransportError::Timeout { target }) })
        }

        fn last_handshake(&self) -> HandshakeFuture<'_> {
            Box::pin(async { None })
        }

        fn find_server_again(&self) -> FindServerFuture<'_> {
            Box::pin(async move {
                let mut lookups = self.lookups.lock().unwrap();
                *lookups += 1;
                Ok(match *lookups {
                    1 => None,
                    2 => Some("192.0.2.20:51820".parse().unwrap()),
                    _ => return Err(TransportError::Setup("no network".to_owned())),
                })
            })
        }
    }

    #[test]
    fn a_silent_server_is_reported_lost_then_looked_for() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        runtime.block_on(async {
            let transport = Silent {
                lookups: Mutex::new(0),
            };
            let said = Arc::new(Mutex::new(Vec::new()));
            let heard = said.clone();
            let watching = watch(&transport, move |message| {
                heard.lock().unwrap().push(message)
            });
            // Paused time jumps ahead whenever everything is waiting.
            let _ = tokio::time::timeout(LOST_AFTER + CHECK_EVERY * 5, watching).await;

            let said = said.lock().unwrap();
            assert!(said[0].starts_with("lost contact"), "{said:?}");
            assert!(said[1].contains("192.0.2.20:51820"), "{said:?}");
            assert!(said[2].contains("no network"), "{said:?}");
            assert_eq!(said.len(), 3, "a failed lookup is said once: {said:?}");
            assert!(*transport.lookups.lock().unwrap() >= 4);
        });
    }
}
