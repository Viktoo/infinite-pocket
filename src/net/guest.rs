//! Joining someone's world: connect, say hello, and take the world in.

use super::wire::{Down, Hello, Traveler, Up, Welcome};
use super::{Link, join_url, spawn_link, this_build};
use anyhow::{Context, Result, bail};
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;

/// In someone's world: the link to the host and what came first.
pub struct Joined {
    pub link: Link<Down, Up>,
    pub welcome: Welcome,
    /// The world's file, whole (it is only ever kept in memory).
    pub world: Vec<u8>,
    pub roster: Vec<Traveler>,
}

/// Connect to a host at `addr` (as typed: an address, or an ngrok URL).
pub fn connect(addr: &str) -> Result<Link<Down, Up>> {
    let url = join_url(addr);
    let req = url.as_str().into_client_request().with_context(|| format!("{addr} isn't an address"))?;
    let uri = req.uri().clone();
    let host = uri.host().context("no host in the address")?.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
    let addrs: Vec<_> = (host.as_str(), port).to_socket_addrs().with_context(|| format!("can't find {host}"))?.collect();
    let mut last = None;
    let mut sock = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(6)) {
            Ok(s) => {
                sock = Some(s);
                break;
            }
            Err(e) => last = Some((e, a)),
        }
    }
    let sock = match sock {
        Some(s) => s,
        None => match last {
            Some((e, a)) => bail!("{}", unreached(&host, port, &e, a.ip())),
            None => bail!("can't reach {host}:{port}"),
        },
    };
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (ws, _) = tungstenite::client_tls_with_config(req, sock, Some(super::ws_config()), None).map_err(|e| anyhow::anyhow!("{host} didn't answer as a game: {e}"))?;
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => super::tune(s)?,
        MaybeTlsStream::Rustls(s) => super::tune(s.get_ref())?,
        _ => {}
    }
    Ok(spawn_link(ws, "guest-link".into()))
}

/// Why `host:port` couldn't be reached, as the one joining can act on it.
fn unreached(host: &str, port: u16, e: &std::io::Error, ip: std::net::IpAddr) -> String {
    use std::io::ErrorKind as K;
    let near = match ip {
        std::net::IpAddr::V4(v) => v.is_private() || v.is_link_local(),
        std::net::IpAddr::V6(_) => false,
    };
    match e.kind() {
        K::HostUnreachable | K::NetworkUnreachable if near && cfg!(target_os = "macos") => format!("Can't reach {host}:{port}: macOS may be keeping this game off the local network. Allow it in System Settings → Privacy & Security → Local Network (it may be listed as your terminal), then try again."),
        K::ConnectionRefused => format!("Nothing is hosting at {host}:{port}. Check the address, and that the host's world is open to friends."),
        K::TimedOut if near => format!("{host}:{port} didn't answer. Check you're both on the same Wi-Fi, and if the host's Mac has its firewall on, that it lets Infinite Pocket in (System Settings → Network → Firewall)."),
        _ => format!("Can't reach {host}:{port} ({e})."),
    }
}

/// Join: hello, then the welcome and the world. `progress` hears how far
/// along (0..1) and what is happening, in words for the loading screen.
pub fn join(addr: &str, code: &str, name: &str, token: &str, mut progress: impl FnMut(f32, &str)) -> Result<Joined> {
    progress(0.0, "connecting…");
    let link = connect(addr)?;
    let (release, commit, protocol) = this_build();
    link.send(Up::Hello(Hello { protocol, release: release.into(), commit: commit.into(), name: name.into(), token: token.into(), code: code.into() }));
    let mut welcome = None;
    let mut packed = Vec::new();
    let mut roster = Vec::new();
    let mut last = Instant::now();
    loop {
        let Some(m) = link.recv_timeout(Duration::from_millis(200)) else {
            if link.is_closed() {
                bail!("the host closed the connection ({})", link.why().unwrap_or_default());
            }
            if last.elapsed() > Duration::from_secs(60) {
                bail!("the host stopped sending");
            }
            continue;
        };
        last = Instant::now();
        match m {
            Down::Refused(why) => bail!("{why}"),
            Down::Welcome(w) => {
                progress(0.0, &format!("joining {}…", w.world));
                welcome = Some(w);
            }
            Down::WorldChunk(b) => {
                packed.extend_from_slice(&b);
                if let Some(w) = &welcome {
                    progress(packed.len() as f32 / w.packed.max(1) as f32, &format!("getting {}… {:.1} of {:.1} MB", w.world, packed.len() as f64 / 1e6, w.packed as f64 / 1e6));
                }
            }
            Down::WorldDone => break,
            Down::Roster(r) => roster = r,
            Down::Bye(why) => bail!("{why}"),
            _ => {}
        }
    }
    let welcome = welcome.context("the host sent the world without a welcome")?;
    let mut world = Vec::with_capacity(welcome.bytes as usize);
    flate2::read::DeflateDecoder::new(&packed[..]).read_to_end(&mut world).context("the world came broken")?;
    if world.len() as u64 != welcome.bytes {
        bail!("the world came short ({} of {} bytes)", world.len(), welcome.bytes);
    }
    Ok(Joined { link, welcome, world, roster })
}

/// The host's world seen from the guest's side: here "the player" is the
/// guest, and the host is traveler 0. What the host holds and wears is
/// theirs (traveler 0's), what the guest left holding is the guest's own;
/// where the guest stands and how they look are as the host keeps them.
pub fn as_guest(db: &crate::db::Db, w: &Welcome) -> Result<()> {
    let you = crate::sim::ActorId::Traveler(w.you).key();
    let host = crate::sim::ActorId::Traveler(0).key();
    db.tx(|c| {
        for col in ["holder", "co_holder"] {
            // Through a name no one has, so the two don't trade places twice.
            c.execute(&format!("UPDATE things SET {col} = '~host' WHERE {col} = 'player'"), [])?;
            c.execute(&format!("UPDATE things SET {col} = 'worn:~host' WHERE {col} = 'worn:player'"), [])?;
            c.execute(&format!("UPDATE things SET {col} = 'player' WHERE {col} = ?1"), [&you])?;
            c.execute(&format!("UPDATE things SET {col} = 'worn:player' WHERE {col} = ?1"), [format!("worn:{you}")])?;
            c.execute(&format!("UPDATE things SET {col} = ?1 WHERE {col} = '~host'"), [&host])?;
            c.execute(&format!("UPDATE things SET {col} = ?1 WHERE {col} = 'worn:~host'"), [format!("worn:{host}")])?;
        }
        c.execute("UPDATE player SET x = ?1, z = ?2, yaw = ?3, t_game = ?4 WHERE id = 1", rusqlite::params![w.body.pos[0], w.body.pos[2], w.body.yaw, w.t])?;
        crate::db::kv_set(c, "player.body", &serde_json::to_string(&crate::sim::hands::OwnBody { look: w.look.clone() })?)?;
        // The host's wounds are theirs; the guest's own come with them.
        if let Some(v) = c.query_row("SELECT value FROM kv WHERE key = 'sim.night'", [], |r| r.get::<_, String>(0)).ok() {
            if let Ok(mut n) = serde_json::from_str::<crate::sim::night::NightState>(&v) {
                n.wounds = w.wounds;
                n.fallen = None;
                n.fallen_at = 0.0;
                n.fell_tonight = false;
                crate::db::kv_set(c, "sim.night", &serde_json::to_string(&n)?)?;
            }
        }
        Ok(())
    })
}
