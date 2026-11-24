//! Self-update from GitHub releases: `qed update` / `qed upgrade`, and a quiet background
//! check in the REPL that installs a new release and offers `/restart`.
//! Uses curl, tar and shasum/sha256sum from the system (no extra crates).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

const REPO: &str = "agusgarcia3007/qed";
/// The background check runs at most this often.
const EVERY: Duration = Duration::from_secs(3600);

/// Version installed by the background check, waiting for /restart.
pub static READY: Mutex<Option<String>> = Mutex::new(None);
static EXE: OnceLock<PathBuf> = OnceLock::new();

/// Remember this binary's path at startup: on Linux, /proc/self/exe points at a deleted
/// file once the binary has been replaced.
pub fn init() {
    EXE.get_or_init(|| {
        std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .unwrap_or_else(|_| PathBuf::from("qed"))
    });
}

pub fn exe() -> PathBuf {
    init();
    EXE.get().cloned().unwrap_or_else(|| PathBuf::from("qed"))
}

fn releases() -> String {
    std::env::var("QED_RELEASES_URL").unwrap_or_else(|_| format!("https://github.com/{}/releases", REPO))
}

/// Release asset suffix for this machine, e.g. `aarch64-apple-darwin`.
fn target() -> Option<String> {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux-musl"
    } else {
        return None;
    };
    let mut arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    // An Intel build running under Rosetta moves to the native one.
    if cfg!(target_os = "macos") && arch == "x86_64" {
        let o = Command::new("/usr/sbin/sysctl").args(["-n", "hw.optional.arm64"]).output();
        if o.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1") {
            arch = "aarch64";
        }
    }
    Some(format!("{}-{}", arch, os))
}

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// Is release `tag` newer than version `cur`?
fn newer(tag: &str, cur: &str) -> bool {
    match (parse_version(tag), parse_version(cur)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Latest release tag, from where `releases/latest` redirects (no API token, no rate limit).
fn latest() -> Result<String, String> {
    let o = Command::new("curl")
        .args(["-fsSL", "-o", "/dev/null", "--max-time", "15", "-w", "%{url_effective}"])
        .arg(format!("{}/latest", releases()))
        .output()
        .map_err(|e| format!("curl: {}", e))?;
    let url = String::from_utf8_lossy(&o.stdout).to_string();
    if !o.status.success() {
        return Err(format!("could not reach {}", releases()));
    }
    url.rsplit_once("/tag/")
        .map(|(_, t)| t.trim().to_string())
        .filter(|t| parse_version(t).is_some())
        .ok_or_else(|| "no release found".to_string())
}

fn sha256(path: &Path) -> Option<String> {
    for (bin, args) in [("shasum", &["-a", "256"][..]), ("sha256sum", &[][..])] {
        if let Ok(o) = Command::new(bin).args(args).arg(path).stderr(Stdio::null()).output() {
            if o.status.success() {
                return String::from_utf8_lossy(&o.stdout).split_whitespace().next().map(String::from);
            }
        }
    }
    None
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let o = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(|e| format!("{}: {}", what, e))?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("{} failed: {}", what, String::from_utf8_lossy(&o.stderr).trim()))
    }
}

/// A binary built from a checkout (cargo build / cargo test) is never replaced.
fn dev_build(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str() == "target")
}

/// Download release `tag`, verify it against SHA256SUMS and that it runs, then swap it in
/// for the running binary (rename in the same directory: atomic, and the running process
/// keeps its old file).
fn install(tag: &str) -> Result<PathBuf, String> {
    let t = target().ok_or("no prebuilt binary for this platform; use cargo install")?;
    let exe = exe();
    let dir = exe.parent().ok_or("cannot locate the qed binary")?;
    let tmp = dir.join(format!(".qed-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)
        .map_err(|e| format!("cannot write to {} ({}); reinstall with the installer or use sudo", dir.display(), e))?;
    let res = (|| {
        let asset = format!("qed-{}.tar.gz", t);
        let base = format!("{}/download/{}", releases(), tag);
        let get = |name: &str| {
            run(
                Command::new("curl").args(["-fsSL", "--max-time", "120", "-o"]).arg(tmp.join(name)).arg(format!("{}/{}", base, name)),
                &format!("download {}", name),
            )
        };
        get(&asset)?;
        get("SHA256SUMS")?;
        let sums = std::fs::read_to_string(tmp.join("SHA256SUMS")).map_err(|e| e.to_string())?;
        let want = sums
            .lines()
            .find(|l| l.split_whitespace().nth(1) == Some(asset.as_str()))
            .and_then(|l| l.split_whitespace().next())
            .ok_or("asset missing from SHA256SUMS")?;
        if sha256(&tmp.join(&asset)).as_deref() != Some(want) {
            return Err("checksum mismatch; not installing".to_string());
        }
        run(Command::new("tar").arg("-xzf").arg(tmp.join(&asset)).arg("-C").arg(&tmp), "extract")?;
        let new = tmp.join("qed");
        let o = Command::new(&new).arg("--version").output().map_err(|e| format!("new binary does not run: {}", e))?;
        if !String::from_utf8_lossy(&o.stdout).starts_with("qed ") {
            return Err("new binary does not run".to_string());
        }
        std::fs::rename(&new, &exe).map_err(|e| format!("replace {}: {}", exe.display(), e))?;
        Ok(exe.clone())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

/// `qed update` / `qed upgrade`.
pub fn cmd() -> i32 {
    let cur = env!("CARGO_PKG_VERSION");
    if dev_build(&exe()) {
        eprintln!("qed: {} is a development build; rebuild it with cargo instead", exe().display());
        return 1;
    }
    let tag = match latest() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("qed: {}", e);
            return 1;
        }
    };
    if !newer(&tag, cur) {
        println!("qed {} is up to date", cur);
        return 0;
    }
    println!("updating qed {} → {}", cur, tag.trim_start_matches('v'));
    match install(&tag) {
        Ok(p) => {
            println!("installed {}", p.display());
            0
        }
        Err(e) => {
            eprintln!("qed: {}", e);
            1
        }
    }
}

fn stamp() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("qed").join("update-check"))
}

/// REPL start: check for a new release in a background thread (at most hourly) and
/// install it; the REPL then offers /restart. Off with QED_NO_UPDATE=1 or
/// `"autoupdate": false` in .qed/config.json, and never for development builds.
pub fn background(enabled: bool) {
    if !enabled || std::env::var_os("QED_NO_UPDATE").is_some() || dev_build(&exe()) {
        return;
    }
    if let Some(p) = stamp() {
        let recent = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < EVERY);
        if recent {
            return;
        }
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, b"");
    }
    std::thread::spawn(|| {
        let Ok(tag) = latest() else { return };
        if newer(&tag, env!("CARGO_PKG_VERSION")) && install(&tag).is_ok() {
            if let Ok(mut r) = READY.lock() {
                *r = Some(tag.trim_start_matches('v').to_string());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("v0.1.10", "0.1.9"));
        assert!(newer("v0.2.0", "0.1.1"));
        assert!(!newer("v0.1.1", "0.1.1"));
        assert!(!newer("v0.1.0", "0.1.1"));
        assert!(!newer("garbage", "0.1.1"));
    }

    #[test]
    fn checkout_builds_are_never_replaced() {
        assert!(dev_build(Path::new("/x/qed/target/release/qed")));
        assert!(!dev_build(Path::new("/Users/a/.local/bin/qed")));
    }
}
