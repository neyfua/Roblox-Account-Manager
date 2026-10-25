use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const SOBER_APP_ID: &str = "org.vinegarhq.Sober";

static PROCESSES: LazyLock<Mutex<HashMap<i64, TrackedProcess>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CHILDREN: LazyLock<Mutex<HashMap<i64, Child>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MULTI_LAUNCH: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize)]
pub struct TrackedProcess {
    pub pid: u32,
    pub user_id: i64,
    pub browser_tracker_id: String,
}

pub fn generate_browser_tracker_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}{}", now % 75_000 + 100_000, now / 31 % 800_000 + 100_000)
}

fn host_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "Cannot determine home directory".into())
}

/// Per-account Sober HOME, reserved for concurrent secondary instances.
pub fn instance_home(user_id: i64) -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("Cannot determine XDG data directory")?;
    let path = base
        .join("roblox-account-manager/sober")
        .join(user_id.to_string());
    std::fs::create_dir_all(&path).map_err(|e| format!("Failed to create Sober profile: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Profiles hold session cookies; keep them owner-only.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to secure Sober profile: {e}"))?;
    }
    seed_profile(&path)?;
    Ok(path)
}

/// A brand-new HOME makes Sober bootstrap the whole Roblox client again, which
/// hangs on its first-run "installation guide" (private D-Bus has no portal to
/// answer its dialogs). Seed the client payload + config from the user's working
/// install so the profile is immediately playable. `cache/` (1.8 GB) is skipped;
/// it rebuilds itself. Session cookies are dropped so profiles stay separate.
fn seed_profile(profile: &std::path::Path) -> Result<(), String> {
    let Some(host_home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(());
    };
    seed_from(&host_home.join(".var/app").join(SOBER_APP_ID), profile)
}

fn write_sober_cookie(profile: &std::path::Path, security_token: &str) -> Result<(), String> {
    let token = security_token.trim().trim_start_matches(".ROBLOSECURITY=");
    if token.is_empty() || token.contains(['\n', '\r', ';']) {
        return Err("Account cookie is empty or malformed".into());
    }
    let cookie_path = profile
        .join(".var/app")
        .join(SOBER_APP_ID)
        .join("data/sober/cookies");
    let parent = cookie_path.parent().ok_or("Invalid Sober cookie path")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Cannot create Sober data directory: {e}"))?;
    let cookie_line = format!(".ROBLOSECURITY={token}");
    std::fs::write(&cookie_path, cookie_line)
        .map_err(|e| format!("Cannot write Sober login cookie: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cookie_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Cannot secure Sober login cookie: {e}"))?;
    }
    Ok(())
}

fn seed_from(host: &std::path::Path, profile: &std::path::Path) -> Result<(), String> {
    let dest = profile.join(".var/app").join(SOBER_APP_ID);
    if dest.join("data/sober/assets").exists() || !host.join("data").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("Failed to create profile dir: {e}"))?;

    for sub in ["data", "config"] {
        let status = Command::new("cp")
            .args(["-a", "--reflink=auto"])
            .arg(host.join(sub))
            .arg(&dest)
            .status()
            .map_err(|e| format!("Failed to seed Sober profile: {e}"))?;
        if !status.success() {
            return Err(format!(
                "Failed to seed Sober profile from {} (cp exited with {status})",
                host.display()
            ));
        }
    }

    // Never clone a live session: identity, logs and crash dumps are per-profile.
    for junk in ["cookies", "state", "sober_logs", "crashes"] {
        let _ = std::fs::remove_dir_all(dest.join("data/sober").join(junk));
        let _ = std::fs::remove_file(dest.join("data/sober").join(junk));
    }
    Ok(())
}

pub fn build_launch_uri(place_id: i64, job_id: &str, launch_data: &str) -> String {
    let mut uri = format!("roblox://experiences/start?placeId={place_id}");
    if !job_id.trim().is_empty() {
        uri.push_str("&gameInstanceId=");
        uri.push_str(&urlencoding::encode(job_id));
    }
    if !launch_data.is_empty() {
        uri.push_str("&launchData=");
        uri.push_str(&urlencoding::encode(launch_data));
    }
    uri
}

/// Sober's sober_services registers `org.vinegarhq.Sober` on the session bus.
/// A second instance on the same bus finds that name already owned and shows a
/// "frozen instance" popup. `--no-session-bus` prevents session-bus forwarding
/// entirely, so the name is never registered and multiple instances coexist.
/// The portals Sober needs for login are bypassed because we pre-seed the
/// cookie directly, so the WebKit login flow never runs.
fn spawn_sober(profile: &PathBuf, uri: &str) -> Result<Child, String> {
    let mut command = Command::new("flatpak");
    command.args(["run", "--no-session-bus", SOBER_APP_ID, uri]);
    command
        .env("HOME", profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "flatpak not found. Install flatpak and org.vinegarhq.Sober.".to_string()
        } else {
            format!("Failed to launch Sober: {e}")
        }
    })
}

/// Kept for future use (e.g. detecting externally-started Sober that may own the D-Bus name).
fn host_sober_running() -> bool {
    Command::new("pgrep")
        .args(["-f", SOBER_APP_ID])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn launch(
    user_id: i64,
    security_token: &str,
    place_id: i64,
    job_id: &str,
    launch_data: &str,
) -> Result<u32, String> {
    let profile = if MULTI_LAUNCH.load(Ordering::Relaxed) && !tracker_all().is_empty() {
        instance_home(user_id)?
    } else {
        host_home()?
    };
    write_sober_cookie(&profile, security_token)?;
    let uri = build_launch_uri(place_id, job_id, launch_data);
    let child = spawn_sober(&profile, &uri)?;
    let pid = child.id();

    CHILDREN
        .lock()
        .map_err(|_| "Sober process state unavailable")?
        .insert(user_id, child);
    PROCESSES
        .lock()
        .map_err(|_| "Sober process state unavailable")?
        .insert(
            user_id,
            TrackedProcess {
                pid,
                user_id,
                browser_tracker_id: generate_browser_tracker_id(),
            },
        );
    Ok(pid)
}

pub fn tracker_all() -> Vec<TrackedProcess> {
    let Ok(mut children) = CHILDREN.lock() else {
        return Vec::new();
    };
    let Ok(mut processes) = PROCESSES.lock() else {
        return Vec::new();
    };
    children.retain(|user_id, child| {
        if child.try_wait().ok().flatten().is_none() {
            true
        } else {
            processes.remove(user_id);
            false
        }
    });
    processes.values().cloned().collect()
}

pub fn kill_for_user(user_id: i64) -> bool {
    let Ok(mut children) = CHILDREN.lock() else {
        return false;
    };
    let Some(mut child) = children.remove(&user_id) else {
        return false;
    };
    let pid = child.id();
    // The child was spawned as a session leader; signal the whole group so the
    // flatpak/bwrap children go down too. Fall back to Child::kill.
    let group_killed = Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    std::thread::sleep(std::time::Duration::from_millis(250));
    if !group_killed || child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    if let Ok(mut processes) = PROCESSES.lock() {
        processes.remove(&user_id);
    }
    group_killed
}

pub fn kill_all() -> u32 {
    let ids: Vec<i64> = CHILDREN
        .lock()
        .map(|children| children.keys().copied().collect())
        .unwrap_or_default();
    ids.into_iter().filter(|id| kill_for_user(*id)).count() as u32
}

pub fn enable_multi_roblox() -> Result<bool, String> {
    MULTI_LAUNCH.store(true, Ordering::Relaxed);
    Ok(true)
}

pub fn disable_multi_roblox() -> Result<(), String> {
    MULTI_LAUNCH.store(false, Ordering::Relaxed);
    Ok(())
}

pub fn multi_launch_enabled() -> bool {
    MULTI_LAUNCH.load(Ordering::Relaxed)
}

pub fn get_roblox_path() -> Result<String, String> {
    let output = Command::new("flatpak")
        .args(["info", "--show-location", SOBER_APP_ID])
        .output()
        .map_err(|e| format!("Failed to query Flatpak: {e}"))?;
    if !output.status.success() {
        return Err("Sober Flatpak is not installed (org.vinegarhq.Sober)".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn apply_fps_unlock(_max_fps: u32) -> Result<(), String> {
    // Roblox enforces an FFlag allowlist since 2025; Sober's client ignores
    // FPS overrides and the value is only adjustable inside the game.
    Err("FPS is set in-game (Esc -> Settings); Sober ignores FFlag overrides".into())
}

pub fn current_session_type() -> String {
    std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "wayland".into()
        } else if std::env::var_os("DISPLAY").is_some() {
            "x11".into()
        } else {
            "unknown".into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_uri_encodes_user_data_and_omits_empty_optional_values() {
        assert_eq!(
            build_launch_uri(42, "", ""),
            "roblox://experiences/start?placeId=42"
        );
        assert_eq!(
            build_launch_uri(42, "abc def", "a&b"),
            "roblox://experiences/start?placeId=42&gameInstanceId=abc%20def&launchData=a%26b"
        );
    }

    #[test]
    fn seed_from_copies_assets_and_strips_host_cookies() {
        let temp = std::env::temp_dir().join(format!("ram-seed-test-{}", std::process::id()));
        let host = temp.join("host");
        let profile = temp.join("profile");
        let host_sober = host.join("data/sober");
        std::fs::create_dir_all(host_sober.join("assets")).unwrap();
        std::fs::create_dir_all(host.join("config/sober")).unwrap();
        std::fs::write(host_sober.join("cookies"), "LEAKED_COOKIE").unwrap();
        std::fs::write(host_sober.join("assets/UniversalApp"), "binary").unwrap();

        seed_from(&host, &profile).unwrap();

        let seeded_sober = profile
            .join(".var/app")
            .join(SOBER_APP_ID)
            .join("data/sober");
        assert!(seeded_sober.join("assets/UniversalApp").exists());
        assert!(
            !seeded_sober.join("cookies").exists(),
            "cookies must not leak into cloned profile"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn write_sober_cookie_writes_correct_format_and_restricts_permissions() {
        let temp = std::env::temp_dir().join(format!("ram-cookie-test-{}", std::process::id()));
        let res = write_sober_cookie(&temp, "_|WARNING:-TEST-COOKIE");
        assert!(res.is_ok());

        let cookie_file = temp
            .join(".var/app")
            .join(SOBER_APP_ID)
            .join("data/sober/cookies");
        assert!(cookie_file.exists());
        let content = std::fs::read_to_string(&cookie_file).unwrap();
        assert_eq!(content, ".ROBLOSECURITY=_|WARNING:-TEST-COOKIE");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&cookie_file).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn write_sober_cookie_rejects_malformed() {
        let temp = std::env::temp_dir().join(format!("ram-bad-cookie-test-{}", std::process::id()));
        assert!(write_sober_cookie(&temp, "").is_err());
        assert!(write_sober_cookie(&temp, "abc;evil=1").is_err());
        assert!(write_sober_cookie(&temp, "abc\nevil=1").is_err());
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn launch_uses_host_home_for_single_instance() {
        let host = host_home().unwrap();
        let expected_cookie = host
            .join(".var/app")
            .join(SOBER_APP_ID)
            .join("data/sober/cookies");

        assert_eq!(
            expected_cookie,
            host.join(".var/app/org.vinegarhq.Sober/data/sober/cookies")
        );
    }
}
