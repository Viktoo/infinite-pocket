//! Finding games on the local network: a host calls out now and then (a
//! small UDP broadcast saying whose world it is and where), and the join
//! screen lists who it hears. Never the code: that is still given by hand.

use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The port hosts call out on.
pub const BEACON_PORT: u16 = 7778;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Call {
    game: String,
    host: String,
    world: String,
    port: u16,
    release: String,
    commit: String,
    /// What the host sends and keeps (`super::wire()`).
    #[serde(default)]
    wire: u32,
}

/// A game heard on the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub host: String,
    pub world: String,
    /// Where to join (`192.168.1.20:7777`).
    pub addr: String,
    /// Can play with ours (else joining would be turned away).
    pub same_build: bool,
    /// The other build, in words, when it isn't ours.
    pub build: String,
}

/// A host calling out until dropped.
pub struct Beacon {
    stop: Arc<AtomicBool>,
}

impl Beacon {
    pub fn start(host: &str, world: &str, port: u16) -> Option<Beacon> {
        let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        s.set_broadcast(true).ok()?;
        let (release, commit, wire) = super::this_build();
        let call = Call { game: "infinite-pocket".into(), host: host.into(), world: world.into(), port, release: release.into(), commit: commit.into(), wire };
        let bytes = serde_json::to_vec(&call).ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        std::thread::Builder::new()
            .name("host-beacon".into())
            .spawn(move || {
                while !st.load(Ordering::Relaxed) {
                    let _ = s.send_to(&bytes, (Ipv4Addr::BROADCAST, BEACON_PORT));
                    // This machine too (a second window trying it out).
                    let _ = s.send_to(&bytes, (Ipv4Addr::LOCALHOST, BEACON_PORT));
                    std::thread::sleep(Duration::from_millis(1000));
                }
            })
            .ok()?;
        Some(Beacon { stop })
    }
}

impl Drop for Beacon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Listening for hosts (while the join screen is up).
pub struct Listen {
    sock: UdpSocket,
    heard: Vec<(Found, Instant)>,
    since: Instant,
}

impl Listen {
    pub fn start() -> Option<Listen> {
        use socket2::{Domain, Protocol, Socket, Type};
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).ok()?;
        // Several windows on one machine may listen at once.
        s.set_reuse_address(true).ok()?;
        #[cfg(unix)]
        s.set_reuse_port(true).ok()?;
        s.set_broadcast(true).ok()?;
        s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, BEACON_PORT)).into()).ok()?;
        s.set_nonblocking(true).ok()?;
        Some(Listen { sock: s.into(), heard: Vec::new(), since: Instant::now() })
    }

    /// How long it has listened.
    pub fn listening_for(&self) -> Duration {
        self.since.elapsed()
    }

    /// The games heard lately, newest calls kept.
    pub fn hosts(&mut self) -> Vec<Found> {
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = self.sock.recv_from(&mut buf) {
            let Ok(c) = serde_json::from_slice::<Call>(&buf[..n]) else { continue };
            if c.game != "infinite-pocket" {
                continue;
            }
            let mine = super::this_build();
            let same_build = c.wire == mine.2;
            let build = if c.release == "dev" { format!("dev {}", c.commit) } else { c.release.clone() };
            let ip = if from.ip().is_loopback() { "127.0.0.1".to_string() } else { from.ip().to_string() };
            let mut f = Found { host: c.host, world: c.world, addr: format!("{ip}:{}", c.port), same_build, build };
            // One entry per game: its call on this machine and on the network
            // are the same game (the network address is the one to give).
            let port = format!(":{}", c.port);
            match self.heard.iter_mut().find(|(h, _)| h.host == f.host && h.world == f.world && h.addr.ends_with(&port)) {
                Some(x) => {
                    if f.addr.starts_with("127.") && !x.0.addr.starts_with("127.") {
                        f.addr = x.0.addr.clone();
                    }
                    *x = (f, Instant::now());
                }
                None => self.heard.push((f, Instant::now())),
            }
        }
        self.heard.retain(|(_, t)| t.elapsed() < Duration::from_secs(4));
        self.heard.iter().map(|(f, _)| f.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_heard_on_this_machine() {
        let Some(mut l) = Listen::start() else { return };
        let _b = Beacon::start("Hal", "a bay", 7790).unwrap();
        let t0 = Instant::now();
        let mut found = Vec::new();
        while t0.elapsed() < Duration::from_secs(3) && found.is_empty() {
            found = l.hosts().into_iter().filter(|f| f.host == "Hal").collect();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].addr.ends_with(":7790") && found[0].same_build);
    }
}
