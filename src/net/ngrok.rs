//! Reaching a host from outside its network: when `ngrok` is installed (and
//! signed in once), hosting starts `ngrok http PORT` and reads the public
//! address from its log. Guests join with that https address and the code.

use parking_lot::Mutex;
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

/// How the tunnel stands.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Starting,
    /// The public address (https://…).
    Up(String),
    /// Why there is none, in words.
    Failed(String),
}

pub struct Tunnel {
    child: Child,
    pub state: Arc<Mutex<State>>,
}

/// Where `ngrok` is, if it is on the PATH (or in the usual places).
pub fn find() -> Option<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(std::path::PathBuf::from));
    dirs.into_iter().map(|d| d.join(format!("ngrok{}", std::env::consts::EXE_SUFFIX))).find(|p| p.is_file())
}

impl Tunnel {
    /// Start a tunnel to `port`; None when ngrok isn't installed.
    pub fn start(port: u16) -> Option<Tunnel> {
        let exe = find()?;
        let mut child = Command::new(exe).args(["http", &port.to_string(), "--log", "stdout", "--log-format", "logfmt"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
        let state = Arc::new(Mutex::new(State::Starting));
        let out = child.stdout.take()?;
        let err = child.stderr.take();
        let st = state.clone();
        let _ = std::thread::Builder::new().name("ngrok-log".into()).spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(s) = read_line(&line) {
                    *st.lock() = s;
                }
            }
            let mut s = st.lock();
            if *s == State::Starting {
                *s = State::Failed("ngrok stopped".into());
            }
        });
        if let Some(err) = err {
            let st = state.clone();
            let _ = std::thread::Builder::new().name("ngrok-err".into()).spawn(move || {
                for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                    if line.to_lowercase().contains("err") && *st.lock() == State::Starting {
                        *st.lock() = State::Failed(plain_error(&line));
                    }
                }
            });
        }
        Some(Tunnel { child, state })
    }

    pub fn state(&self) -> State {
        self.state.lock().clone()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What one line of ngrok's log says about the tunnel, if anything.
fn read_line(line: &str) -> Option<State> {
    let field = |k: &str| {
        let at = line.find(&format!(" {k}="))? + k.len() + 2;
        let rest = &line[at..];
        Some(if let Some(q) = rest.strip_prefix('"') { q.split('"').next().unwrap_or("").to_string() } else { rest.split_whitespace().next().unwrap_or("").to_string() })
    };
    if let Some(url) = field("url").filter(|u| u.starts_with("https://")) {
        return Some(State::Up(url));
    }
    if field("lvl").is_some_and(|l| l == "eror" || l == "crit") {
        return Some(State::Failed(plain_error(&field("err").unwrap_or_else(|| line.to_string()))));
    }
    None
}

/// ngrok's errors, as a player can act on them.
fn plain_error(e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("authtoken") || l.contains("authentication") {
        "ngrok needs signing in once: run `ngrok config add-authtoken <your token>` (from dashboard.ngrok.com)".into()
    } else if l.contains("limited to 1 simultaneous") || l.contains("already online") {
        "ngrok is already running a tunnel elsewhere (the free plan has one at a time)".into()
    } else {
        let short: String = e.chars().take(120).collect();
        format!("ngrok: {short}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_address_is_read_from_ngroks_log() {
        let up = r#"t=2026-10-17T20:00:00+0200 lvl=info msg="started tunnel" obj=tunnels name=command_line addr=http://localhost:7777 url=https://ab12-34.ngrok-free.app"#;
        assert_eq!(read_line(up), Some(State::Up("https://ab12-34.ngrok-free.app".into())));
        let err = r#"t=2026-10-17T20:00:00+0200 lvl=eror msg="failed to reconnect session" obj=tunnels.session err="authentication failed: Usage of ngrok requires a verified account and authtoken.""#;
        assert!(matches!(read_line(err), Some(State::Failed(m)) if m.contains("add-authtoken")));
        assert_eq!(read_line(r#"t=x lvl=info msg="client session established" obj=tunnels.session"#), None);
    }
}
