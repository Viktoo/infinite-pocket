//! Playing together: one player hosts a world and the others join it.
//!
//! The host runs the whole world and every LLM call; guests send what they
//! do and are sent what happens. One WebSocket per guest (plain on the local
//! network, through an ngrok tunnel's https address from outside), carrying
//! binary messages (`wire`). Each connection runs on its own thread, so the
//! game's main thread only ever takes what has come in and hands over what
//! is to go out; it never waits on the network.
//!
//! See `docs/multiplayer-plan.md`.

pub mod discover;
pub mod guest;
pub mod host;
pub mod ngrok;
pub mod rows;
pub mod update;
pub mod wire;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tungstenite::{Message, WebSocket};

/// The messages' own version, bumped by hand when what a message means
/// changes but not its shape. With a hash of the messages and the world's
/// tables (`POCKET_WIRE`, from build.rs) it makes `wire()`, which host and
/// guest must agree on; their release and commit may differ.
pub const PROTOCOL: u32 = 1;

/// What this build sends and keeps, as one number (see `PROTOCOL`).
pub fn wire() -> u32 {
    let w: u32 = env!("POCKET_WIRE").parse().unwrap_or(0);
    w ^ PROTOCOL.wrapping_mul(0x9e37_79b9)
}

/// The port a host listens on unless told otherwise.
pub const PORT: u16 = 7777;

/// Biggest message either side takes (the world, sent whole, is in chunks).
const MAX_MESSAGE: usize = 32 << 20;

/// One side of a connection, as the game sees it: what came in, and a way
/// to send. The socket itself lives on the connection's thread.
pub struct Link<In, Out> {
    rx: Receiver<In>,
    tx: Sender<Out>,
    closed: Arc<AtomicBool>,
    /// Why it closed, when it did.
    pub why: Arc<parking_lot::Mutex<Option<String>>>,
}

impl<In, Out> Link<In, Out> {
    pub fn send(&self, m: Out) {
        let _ = self.tx.send(m);
    }

    /// A way to send from another thread.
    pub fn sender(&self) -> Sender<Out> {
        self.tx.clone()
    }

    /// How many messages wait to go out: many means the other side's
    /// connection is slower than what is sent to it.
    pub fn backlog(&self) -> usize {
        self.tx.len()
    }

    /// Everything that has come in since last asked.
    pub fn drain(&self) -> Vec<In> {
        self.rx.try_iter().collect()
    }

    pub fn recv_timeout(&self, d: Duration) -> Option<In> {
        self.rx.recv_timeout(d).ok()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) && self.rx.is_empty()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }

    pub fn why(&self) -> Option<String> {
        self.why.lock().clone()
    }
}

/// Encode a message (postcard; large ones are deflated).
pub fn encode<T: serde::Serialize>(m: &T) -> Vec<u8> {
    postcard::to_allocvec(m).unwrap_or_default()
}

pub fn decode<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T> {
    Ok(postcard::from_bytes(b)?)
}

/// Run a connection on its own thread: sends what is handed to it,
/// passes on what comes in, until either side closes it. `stream` must
/// have a short read timeout set, so the loop turns over.
pub fn spawn_link<S, In, Out>(mut ws: WebSocket<S>, name: String) -> Link<In, Out>
where
    S: Read + Write + Send + 'static,
    In: serde::de::DeserializeOwned + Send + 'static,
    Out: serde::Serialize + Send + 'static,
{
    let (in_tx, in_rx) = crossbeam_channel::unbounded::<In>();
    let (out_tx, out_rx) = crossbeam_channel::unbounded::<Out>();
    let closed = Arc::new(AtomicBool::new(false));
    let why = Arc::new(parking_lot::Mutex::new(None));
    let (c, w) = (closed.clone(), why.clone());
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        let end = |reason: String| {
            *w.lock() = Some(reason);
            c.store(true, Ordering::Relaxed);
        };
        let mut last_heard = std::time::Instant::now();
        let mut last_ping = std::time::Instant::now();
        loop {
            let closing = c.load(Ordering::Relaxed);
            // Out first: everything waiting, then one flush.
            let mut wrote = false;
            for m in out_rx.try_iter() {
                match ws.write(Message::Binary(encode(&m).into())) {
                    Ok(()) => {}
                    // A slow socket: the message is kept and goes with the flush.
                    Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        end(format!("sending: {e}"));
                        return;
                    }
                }
                wrote = true;
            }
            if last_ping.elapsed() > Duration::from_secs(5) {
                last_ping = std::time::Instant::now();
                let _ = ws.write(Message::Ping(Vec::new().into()));
                wrote = true;
            }
            if wrote {
                match ws.flush() {
                    Ok(()) => {}
                    Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        end(format!("sending: {e}"));
                        return;
                    }
                }
            }
            // Closed from this side: what was waiting has gone, then goodbye.
            if closing {
                let _ = ws.close(None);
                let _ = ws.flush();
                if w.lock().is_none() {
                    *w.lock() = Some("closed".into());
                }
                return;
            }
            match ws.read() {
                Ok(Message::Binary(b)) => {
                    last_heard = std::time::Instant::now();
                    match decode::<In>(&b) {
                        Ok(m) => {
                            if in_tx.send(m).is_err() {
                                end("the game let go".into());
                                return;
                            }
                        }
                        Err(e) => {
                            end(format!("a message didn't make sense ({e}): are both on the same build?"));
                            return;
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    end("closed".into());
                    return;
                }
                Ok(_) => last_heard = std::time::Instant::now(),
                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                    end("closed".into());
                    return;
                }
                Err(e) => {
                    end(format!("{e}"));
                    return;
                }
            }
            if last_heard.elapsed() > Duration::from_secs(20) {
                end("nothing heard for 20 s".into());
                return;
            }
        }
    });
    Link { rx: in_rx, tx: out_tx, closed, why }
}

/// The socket options every connection uses: a short read timeout (so a
/// connection's thread turns over to send), no delay on small messages.
fn tune(s: &std::net::TcpStream) -> std::io::Result<()> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_millis(4)))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    Ok(())
}

/// The websocket settings: big enough for a chunk of the world.
fn ws_config() -> tungstenite::protocol::WebSocketConfig {
    tungstenite::protocol::WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE))
}

/// A build in words: its release, or `dev` and its commit.
fn build_name((r, c, _): (&str, &str, u32)) -> String {
    if r == "dev" { format!("dev {c}") } else { r.to_string() }
}

/// Why two builds can't play together, in words for the one who joins, or
/// None when they can: when they send and keep the same (`wire()`), whatever
/// their release and commit. Each is (release, commit, wire).
pub fn check_build(host: (&str, &str, u32), guest: (&str, &str, u32)) -> Option<String> {
    if host.2 == guest.2 {
        return None;
    }
    let (h, g) = (build_name(host), build_name(guest));
    if h == g {
        // Same name, different insides (two dev builds of one commit can't
        // differ; a protocol can, in a build with changes not committed).
        return Some(format!("The host's game ({h}) and yours differ inside. You both need the same build to play together."));
    }
    let newer = match (semver(host.0), semver(guest.0)) {
        (Some(a), Some(b)) if a > b => Some(true),
        (Some(a), Some(b)) if a < b => Some(false),
        _ => None,
    };
    Some(match newer {
        Some(true) => format!("The host has Infinite Pocket {h} and you have {g}. Update to {h} to play together."),
        Some(false) => format!("The host has Infinite Pocket {h} and you have {g}. The host needs to update to {g} to play together."),
        None => format!("The host has Infinite Pocket {h} and you have {g}. You both need the same build to play together."),
    })
}

/// Two builds let to play together that aren't the same one, said to `mine`
/// about `who` on `theirs`; None when they are the same.
pub fn build_note(who: &str, mine: (&str, &str, u32), theirs: (&str, &str, u32)) -> Option<String> {
    let (m, t) = (build_name(mine), build_name(theirs));
    (m != t).then(|| format!("{who} on {t} and you on {m}: you can play together, though small things may differ."))
}

/// "0.4.2" as numbers, for which is newer.
pub fn semver(v: &str) -> Option<(u32, u32, u32)> {
    let mut p = v.trim_start_matches('v').split('.').map(|x| x.parse::<u32>().ok());
    Some((p.next()??, p.next()??, p.next().flatten().unwrap_or(0)))
}

/// This build, as `check_build` compares it.
pub fn this_build() -> (&'static str, &'static str, u32) {
    (crate::db::RELEASE, crate::db::COMMIT, wire())
}

/// A short secret a guest must give to join: two words and a number, easy
/// to read out ("lime-otter-42").
pub fn new_code() -> String {
    const A: [&str; 16] = ["lime", "amber", "brave", "calm", "dusty", "early", "fern", "gold", "hazel", "ivory", "jolly", "keen", "lucky", "misty", "noble", "olive"];
    const B: [&str; 16] = ["otter", "heron", "fox", "lark", "moth", "newt", "owl", "pike", "quail", "raven", "seal", "trout", "vole", "wren", "yak", "zebra"];
    let t = crate::settings::new_token();
    let n = u64::from_str_radix(&t[..12], 16).unwrap_or(7);
    format!("{}-{}-{}", A[(n & 15) as usize], B[((n >> 4) & 15) as usize], 10 + (n >> 8) % 90)
}

/// Codes match whatever the case and the separators ("Lime Otter 42").
pub fn same_code(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect::<String>();
    !norm(a).is_empty() && norm(a) == norm(b)
}

/// This machine's address on the local network, if it has one.
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // No packet is sent: connecting a UDP socket only picks the route.
    s.connect("192.168.0.1:9").or_else(|_| s.connect("10.0.0.1:9")).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// What a player types to join, made into a websocket URL: `host:port`,
/// `host` (the usual port), `https://x.ngrok.app` (→ `wss://`), or a
/// `ws://`/`wss://` URL as it is.
pub fn join_url(addr: &str) -> String {
    let a = addr.trim().trim_end_matches('/');
    if let Some(rest) = a.strip_prefix("https://") {
        return format!("wss://{rest}");
    }
    if let Some(rest) = a.strip_prefix("http://") {
        return format!("ws://{rest}");
    }
    if a.starts_with("ws://") || a.starts_with("wss://") {
        return a.to_string();
    }
    // A name without a port: an ngrok-like domain goes over TLS, anything
    // else is a machine on the network at the usual port.
    let has_port = a.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok());
    if has_port {
        format!("ws://{a}")
    } else if a.contains("ngrok") {
        format!("wss://{a}")
    } else {
        format!("ws://{a}:{PORT}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_that_differ_say_who_should_update() {
        assert_eq!(check_build(("0.5.0", "abc", 1), ("0.5.0", "abc", 1)), None);
        // Sending and keeping the same, any two builds play.
        assert_eq!(check_build(("dev", "abc-dirty", 1), ("0.5.0", "def", 1)), None);
        let m = build_note("The host is", ("0.5.0", "def", 1), ("dev", "abc-dirty", 1)).unwrap();
        assert!(m.contains("dev abc-dirty") && m.contains("0.5.0"), "{m}");
        assert_eq!(build_note("The host is", ("0.5.0", "def", 1), ("0.5.0", "def", 1)), None);
        let m = check_build(("0.5.0", "abc", 1), ("0.4.2", "def", 2)).unwrap();
        assert!(m.contains("Update to 0.5.0"), "{m}");
        let m = check_build(("0.4.2", "abc", 1), ("0.5.0", "def", 2)).unwrap();
        assert!(m.contains("host needs to update to 0.5.0"), "{m}");
        let m = check_build(("dev", "abc", 1), ("dev", "def", 2)).unwrap();
        assert!(m.contains("dev abc") && m.contains("dev def") && m.contains("same build"), "{m}");
        assert!(check_build(("dev", "abc", 1), ("dev", "abc", 2)).unwrap().contains("differ inside"));
    }

    #[test]
    fn addresses_become_websocket_urls() {
        assert_eq!(join_url("192.168.1.20:7777"), "ws://192.168.1.20:7777");
        assert_eq!(join_url("192.168.1.20"), "ws://192.168.1.20:7777");
        assert_eq!(join_url("https://abc.ngrok.app/"), "wss://abc.ngrok.app");
        assert_eq!(join_url("abc.ngrok-free.app"), "wss://abc.ngrok-free.app");
        assert_eq!(join_url("ws://localhost:9000"), "ws://localhost:9000");
    }

    #[test]
    fn codes_are_read_loosely() {
        let c = new_code();
        assert_eq!(c.split('-').count(), 3, "{c}");
        assert!(same_code("lime-otter-42", "Lime Otter 42"));
        assert!(!same_code("lime-otter-42", "lime-otter-43"));
        assert!(!same_code("", ""));
    }
}
