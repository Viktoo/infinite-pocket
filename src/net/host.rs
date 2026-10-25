//! Hosting: a listener that lets guests in (the right code, the same
//! build), and the connections of those let in. The game takes each new
//! guest from `arrivals` and talks to them through their link.

use super::wire::{Down, Hello, Up};
use super::{Link, check_build, same_code, spawn_link, this_build};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A guest let in, not yet welcomed by the game.
pub struct Arrival {
    pub hello: Hello,
    pub link: Link<Up, Down>,
    /// Where they connect from (for the log).
    pub from: String,
}

pub struct Host {
    pub port: u16,
    pub code: String,
    arrivals: Receiver<Arrival>,
    stop: Arc<AtomicBool>,
}

impl Host {
    /// Listen on `port` (or the next free one of the few after it).
    pub fn start(port: u16, code: String) -> Result<Host> {
        let (listener, port) = (port..port.saturating_add(10)).find_map(|p| TcpListener::bind(("0.0.0.0", p)).ok().map(|l| (l, p))).with_context(|| format!("no free port from {port}"))?;
        listener.set_nonblocking(true)?;
        let (tx, rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let (s, c) = (stop.clone(), code.clone());
        std::thread::Builder::new().name("host-listen".into()).spawn(move || listen(listener, c, tx, s))?;
        crate::log::info(format!("hosting on port {port}"));
        Ok(Host { port, code, arrivals: rx, stop })
    }

    /// Guests let in since last asked.
    pub fn arrivals(&self) -> Vec<Arrival> {
        self.arrivals.try_iter().collect()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// The address to give on the local network.
    pub fn lan_address(&self) -> String {
        match super::lan_ip() {
            Some(ip) => format!("{ip}:{}", self.port),
            None => format!("localhost:{}", self.port),
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
    }
}

fn listen(l: TcpListener, code: String, tx: Sender<Arrival>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match l.accept() {
            Ok((sock, from)) => {
                let (code, tx) = (code.clone(), tx.clone());
                // Each in turn on its own thread: a slow one holds no one up.
                let _ = std::thread::Builder::new().name("host-greet".into()).spawn(move || {
                    if let Err(e) = greet(sock, from.to_string(), &code, &tx) {
                        crate::log::info(format!("a guest from {from} didn't get in: {e:#}"));
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                crate::log::error(format!("hosting: {e}"));
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// The websocket handshake, then their hello: let in, or told why not.
fn greet(sock: std::net::TcpStream, from: String, code: &str, tx: &Sender<Arrival>) -> Result<()> {
    sock.set_nonblocking(false)?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    let ws = tungstenite::accept_with_config(sock, Some(super::ws_config())).map_err(|e| anyhow::anyhow!("handshake: {e}"))?;
    super::tune(ws.get_ref())?;
    let link: Link<Up, Down> = spawn_link(ws, "host-guest".into());
    let hello = match link.recv_timeout(Duration::from_secs(10)) {
        Some(Up::Hello(h)) => h,
        _ => {
            link.close();
            anyhow::bail!("no hello");
        }
    };
    let refuse = |why: String| {
        link.send(Down::Refused(why.clone()));
        link.close();
        anyhow::anyhow!(why)
    };
    if let Some(why) = check_build(this_build(), (&hello.release, &hello.commit, hello.protocol)) {
        return Err(refuse(why));
    }
    if !same_code(code, &hello.code) {
        return Err(refuse("That code isn't this game's. Ask the host for the code again.".into()));
    }
    if hello.token.trim().is_empty() {
        return Err(refuse("No player token.".into()));
    }
    tx.send(Arrival { hello, link, from }).map_err(|_| anyhow::anyhow!("the game stopped hosting"))?;
    Ok(())
}

/// The world as a guest gets it: the file as it is now (the game saves
/// first), less what is only the host's (what it cost them, the world's
/// own id), deflated. Nothing is left on disk.
pub fn guest_copy(src: &Path) -> Result<(Vec<u8>, u64, super::rows::Snapshot)> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let out: PathBuf = std::env::temp_dir().join(format!("pocket-guest-copy-{}-{}.pocket", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_file(&out);
    let made = (|| -> Result<(Vec<u8>, super::rows::Snapshot)> {
        let c = rusqlite::Connection::open_with_flags(src, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        c.execute("VACUUM INTO ?1", [out.to_string_lossy()])?;
        drop(c);
        let c = rusqlite::Connection::open(&out)?;
        c.execute("DELETE FROM llm_usage", [])?;
        c.execute("DELETE FROM kv WHERE key = 'world_id'", [])?;
        // Who each guest is stays with the host.
        c.execute("UPDATE travelers SET token = 'traveler-' || no", [])?;
        // What the guest has of the world's own tables, to keep it the same from.
        let (had, _) = super::rows::read(&c)?;
        drop(c);
        Ok((std::fs::read(&out)?, had))
    })();
    let _ = std::fs::remove_file(&out);
    let (raw, had) = made.context("copy the world")?;
    let size = raw.len() as u64;
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&raw)?;
    Ok((z.finish()?, size, had))
}

/// The pieces a world is sent in.
pub const CHUNK: usize = 256 << 10;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, PlayerRow};
    use crate::net::guest;
    use crate::net::wire::{Body, Welcome};

    fn world(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pocket-host-{name}-{}.pocket", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let db = Db::create(&p, 7, "a quiet bay", "{}").unwrap();
        db.save_player(PlayerRow { x: 1.0, z: 2.0, yaw: 0.0, t_game: 300.0 }).unwrap();
        db.add_usage("builder", 10, 10, 0.5).unwrap();
        db.with(|c| {
            c.execute("INSERT INTO things(type_id, x, y, z, yaw, scale, params_json, state_json, props_json, holder, co_holder, asleep, anchored, origin_json, born) VALUES (1, 0, 0, 0, 0, 1, '[]', '[]', '{}', 'player', NULL, 1, 0, '{}', 0)", [])?;
            c.execute("INSERT INTO things(type_id, x, y, z, yaw, scale, params_json, state_json, props_json, holder, co_holder, asleep, anchored, origin_json, born) VALUES (1, 0, 0, 0, 0, 1, '[]', '[]', '{}', 'worn:player:2', NULL, 1, 0, '{}', 0)", [])?;
            Ok(())
        })
        .unwrap();
        p
    }

    fn port() -> u16 {
        41000 + (std::process::id() % 2000) as u16 * 10 % 20000
    }

    /// What a game would do with an arrival: the world, in pieces.
    fn welcome(a: &Arrival, path: &Path) {
        let (packed, bytes, _) = guest_copy(path).unwrap();
        a.link.send(Down::Welcome(Welcome { you: 2, world: "a quiet bay".into(), name: "Ana".into(), host: "Hal".into(), bytes, packed: packed.len() as u64, body: Body { pos: [5.0, 0.0, 6.0], ..Default::default() }, look: Default::default(), wounds: 0.2, t: 300.0, seed: 1, build: Default::default() }));
        for c in packed.chunks(1000) {
            a.link.send(Down::WorldChunk(c.to_vec()));
        }
        a.link.send(Down::WorldDone);
    }

    #[test]
    fn a_guest_joins_and_gets_the_world_without_what_is_the_hosts() {
        let path = world("join");
        let host = Host::start(port(), "lime-otter-42".into()).unwrap();
        let addr = format!("127.0.0.1:{}", host.port);
        let p = path.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let t = std::thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < until {
                if let Some(a) = host.arrivals().pop() {
                    welcome(&a, &p);
                    let _ = tx.send(a.hello.name.clone());
                    // Keep the link while the guest reads.
                    std::thread::sleep(Duration::from_millis(500));
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let j = guest::join(&addr, "Lime Otter 42", "Ana", "tok-ana", |_, _| {}).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "Ana");
        assert_eq!(j.welcome.you, 2);
        let db = Db::from_bytes(&j.world).unwrap();
        guest::as_guest(&db, &j.welcome).unwrap();
        db.with(|c| {
            let spent: i64 = c.query_row("SELECT COUNT(*) FROM llm_usage", [], |r| r.get(0))?;
            assert_eq!(spent, 0, "what the world cost the host stays with them");
            let holders: String = c.query_row("SELECT group_concat(holder, ' ') FROM things ORDER BY id", [], |r| r.get(0))?;
            assert_eq!(holders, "player:0 worn:player", "the host's things are traveler 0's, the guest's are the player's");
            let (x, z): (f64, f64) = c.query_row("SELECT x, z FROM player", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            assert_eq!((x, z), (5.0, 6.0), "the guest stands where the host says");
            Ok(())
        })
        .unwrap();
        t.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_wrong_code_or_another_build_is_turned_away() {
        let host = Host::start(port() + 1000, "lime-otter-42".into()).unwrap();
        let addr = format!("127.0.0.1:{}", host.port);
        let e = guest::join(&addr, "lime-otter-43", "Ana", "tok", |_, _| {}).err().unwrap();
        assert!(e.to_string().contains("code"), "{e}");
        // Another build says so before anything is sent.
        let link = guest::connect(&addr).unwrap();
        link.send(Up::Hello(crate::net::wire::Hello { protocol: crate::net::PROTOCOL, release: "0.0.1".into(), commit: "abc".into(), name: "Old".into(), token: "t".into(), code: "lime-otter-42".into() }));
        match link.recv_timeout(Duration::from_secs(5)) {
            Some(Down::Refused(why)) => assert!(why.contains("0.0.1"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(host.arrivals().is_empty());
    }
}
