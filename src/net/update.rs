//! Whether a newer release is out: asked of GitHub once a run (when the
//! settings allow), in the background, and only for a release build (a
//! dev build is someone's own). The worlds list says so; nothing updates
//! by itself.

use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, channel};

/// Where releases are published.
pub const REPO: &str = "Viktoo/infinite-pocket";

/// A release newer than this one.
#[derive(Clone, Debug, PartialEq)]
pub struct Newer {
    pub version: String,
    pub url: String,
}

/// Installed with Homebrew (it lives in a Cellar): it updates with
/// `brew upgrade pocket`, not from the release page.
pub fn from_brew() -> bool {
    std::env::current_exe().and_then(|p| p.canonicalize()).is_ok_and(|p| p.components().any(|c| c.as_os_str() == "Cellar"))
}

static ASKED: OnceLock<parking_lot::Mutex<Receiver<Option<Newer>>>> = OnceLock::new();
static FOUND: OnceLock<Option<Newer>> = OnceLock::new();

/// Start asking (once a run).
pub fn ask() {
    if crate::db::RELEASE == "dev" || cfg!(test) || !crate::settings::Settings::load().check_updates {
        return;
    }
    ASKED.get_or_init(|| {
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
            let _ = tx.send(latest().ok().flatten());
        });
        parking_lot::Mutex::new(rx)
    });
}

/// A newer release, once the answer came (None until then, or when there is none).
pub fn newer() -> Option<Newer> {
    if let Some(f) = FOUND.get() {
        return f.clone();
    }
    let got = ASKED.get()?.lock().try_recv().ok()?;
    let _ = FOUND.set(got.clone());
    got
}

fn latest() -> anyhow::Result<Option<Newer>> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let v: serde_json::Value = rt.block_on(async {
        let c = reqwest::Client::builder().user_agent(format!("infinite-pocket/{}", crate::db::RELEASE)).timeout(std::time::Duration::from_secs(8)).build()?;
        let r = c.get(format!("https://api.github.com/repos/{REPO}/releases/latest")).header("Accept", "application/vnd.github+json").send().await?.error_for_status()?;
        anyhow::Ok(r.json().await?)
    })?;
    let tag = v["tag_name"].as_str().unwrap_or_default();
    let url = v["html_url"].as_str().unwrap_or_default().to_string();
    Ok(is_newer(tag, crate::db::RELEASE).then(|| Newer { version: tag.trim_start_matches('v').to_string(), url }))
}

/// Whether release `tag` (v0.5.0) is newer than `mine` (0.4.2).
pub fn is_newer(tag: &str, mine: &str) -> bool {
    matches!((super::semver(tag), super::semver(mine)), (Some(a), Some(b)) if a > b)
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_later_release_is_newer() {
        assert!(super::is_newer("v0.5.0", "0.4.2"));
        assert!(!super::is_newer("v0.4.2", "0.4.2"));
        assert!(!super::is_newer("v0.4.1", "0.4.2"));
        assert!(!super::is_newer("nightly", "0.4.2"));
    }

    /// Built from source, the tag the README names: this release, so it plays with brew's.
    #[test]
    fn readme_installs_this_release() {
        let tag = format!("--tag v{}", env!("CARGO_PKG_VERSION"));
        assert!(include_str!("../../README.md").contains(&tag), "README's build from source should say {tag}");
    }
}
