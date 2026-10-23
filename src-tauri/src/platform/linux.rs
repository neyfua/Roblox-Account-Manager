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

/// Per-account Sober HOME. Isolating HOME isolates `~/.var/app/...` (login,
/// config, cache) so two clients do not share the same Roblox session.
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
    Ok(path)
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

/// Sober claims a well-known D-Bus name, so a second process on the same bus
/// exits with a "frozen instance" popup. A private session bus per instance is
/// what makes concurrent clients survive.
fn spawn_sober(profile: &PathBuf, uri: &str) -> Result<Child, String> {
    let mut command = Command::new("dbus-run-session");
    command.args(["--", "flatpak", "run", SOBER_APP_ID, uri]);
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
            "dbus-run-session not found. Install dbus-daemon to launch Sober with an isolated session bus.".to_string()
        } else {
            format!("Failed to launch Sober in isolated D-Bus session: {e}")
        }
    })
}

pub fn launch(user_id: i64, place_id: i64, job_id: &str, launch_data: &str) -> Result<u32, String> {
    let profile = instance_home(user_id)?;
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
    use super::build_launch_uri;

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
}
