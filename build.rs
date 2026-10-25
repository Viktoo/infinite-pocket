//! Puts the git commit the game is built from into it (`POCKET_COMMIT`), so
//! a world can say which build made it and which last opened it, and the
//! release it is (`POCKET_RELEASE`: `0.1.0` for a clean build at tag
//! `v0.1.0`, otherwise `dev`).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A hash of `src/net/wire.rs`, `src/net/rows.rs` and the world's schema
/// (in `src/db.rs`), less comments, blank lines and tests.
fn wire() -> u32 {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let code = |s: &str| s.split("#[cfg(test)]").next().unwrap_or("").to_string();
    let db = read("src/db.rs");
    let schema = db.split("const SCHEMA: &str = r#\"").nth(1).and_then(|s| s.split("\"#;").next()).unwrap_or("").to_string();
    let mut h: u32 = 0x811c_9dc5;
    for part in [code(&read("src/net/wire.rs")), code(&read("src/net/rows.rs")), schema] {
        for line in part.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//")) {
            for b in line.bytes().chain([b'\n']) {
                h = (h ^ b as u32).wrapping_mul(0x0100_0193);
            }
        }
    }
    h
}

fn main() {
    let commit = match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(h) => {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{h}-dirty") } else { h }
        }
        None => "unknown".into(),
    };
    println!("cargo:rustc-env=POCKET_COMMIT={commit}");
    // The release: the version, when the build is a clean one of its tag
    // (`v0.1.0`); any other build is "dev".
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let tagged = git(&["tag", "--points-at", "HEAD"]).is_some_and(|t| t.lines().any(|l| l.trim() == format!("v{version}")));
    let release = if tagged && !commit.ends_with("-dirty") && commit != "unknown" { version.clone() } else { "dev".into() };
    println!("cargo:rustc-env=POCKET_RELEASE={release}");
    // What two games playing together must agree on: the messages they send
    // and the world file's tables. Any other change (a commit, a tweak not
    // committed) still lets them play.
    println!("cargo:rustc-env=POCKET_WIRE={}", wire());
    // Again when the commit or the code changes.
    if let (Some(dir), Some(common)) = (git(&["rev-parse", "--git-dir"]), git(&["rev-parse", "--git-common-dir"])) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/index");
        println!("cargo:rerun-if-changed={common}/refs/tags");
        println!("cargo:rerun-if-changed={common}/packed-refs");
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={common}/{r}");
        }
    }
    // On a Mac the binary carries its own Info.plist: who the game is and why
    // it asks for the microphone or the local network, which the system shows
    // when it is played from a terminal (src/own_app.rs).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("Info.plist");
        std::fs::write(&plist, format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.infinitepocket.game</string>
  <key>CFBundleName</key><string>Infinite Pocket</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>NSMicrophoneUsageDescription</key><string>Speak to talk listens to you when you turn it on in settings.</string>
  <key>NSLocalNetworkUsageDescription</key><string>Finds friends' games on your network, and lets them join yours.</string>
</dict>
</plist>
"#)).unwrap();
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    }
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
