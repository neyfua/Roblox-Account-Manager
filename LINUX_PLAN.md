# Linux (Wayland/Sober) Support Plan for Roblox Account Manager

## Understanding

### Current State
- **Roblox Account Manager** (this repo): Tauri v2 app (Rust + React/TypeScript)
  - Has Windows (`platform/windows/`) and macOS (`platform/macos/`) backends
  - Linux platform module does NOT exist — all Linux `cfg` blocks return "Not supported"
  - Settings already have a `[Linux]` section with defaults: `PreferredRunner=sober`, `CustomLaunchCommand`, `CustomProcessMatch`, `EnableExperimentalMultiRbx`
  - Frontend already has `PlatformCapabilities` interface with Linux-aware guards
  - Frontend invokes `get_platform_capabilities` command (NOT yet implemented in Rust)
  - Many `#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]` stubs return errors

- **Linux-Roblox-Account-Manager** (reference): Docker-based Python tool
  - Uses Docker containers to isolate Sober instances (each gets own HOME, D-Bus, network)
  - Each container runs flatpak Sober inside XFCE+VNC
  - Heavy: ~3GB per instance, privileged containers

### Multi-Instance Strategy for Linux
Based on research, Sober can be multi-instanced WITHOUT Docker by using `env HOME=<isolated_dir> flatpak run org.vinegarhq.Sober` (as seen in SoberLauncher and Withdrawal projects). Each instance gets its own:
- HOME directory (isolated Sober data)
- Flatpak runtime sandbox (via bubblewrap)

No need for Docker. Each instance uses ~500-700MB (Sober data clone).

## Implementation Plan

### Phase 1: Core Linux Platform Module
Create `src-tauri/src/platform/linux/` with these files:

1. **`core.rs`** — Platform detection, Sober path discovery, helpers
   - `detect_session_type()` → "wayland" | "x11" | "tty"
   - `detect_sober_installation()` → path to Sober flatpak
   - `home_dir()`, `generate_browser_tracker_id()`
   - `get_sober_config_dir()` → `~/.var/app/org.vinegarhq.Sober/config/sober/`
   - `get_sober_log_dir()` → `~/.var/app/org.vinegarhq.Sober/data/sober/sober_logs/`

2. **`launch.rs`** — Single-instance Sober launch
   - `launch_sober(cookie, place_id, job_id, ...)` — Set auth cookie, launch via flatpak
   - `build_launch_url(...)` — Reuse same URL format as macOS
   - `launch_url(url)` — `flatpak run org.vinegarhq.Sober <url>` or `xdg-open`

3. **`multi.rs`** — Multi-instance via HOME isolation
   - `get_instances_dir()` → `~/.local/share/roblox-account-manager/instances/`
   - `ensure_instance_home(instance_id)` — Create/reuse isolated HOME
   - `launch_isolated(instance_id, url)` — `env HOME=<dir> flatpak run org.vinegarhq.Sober`
   - `clone_sober_data(from, to)` — Clone base Sober data for fast startup
   - Instance lifecycle: list, create, remove

4. **`tracker.rs`** — ProcessTracker (same interface as macOS tracker)
   - Track PID ↔ user_id mappings
   - `get_roblox_pids()` — `pgrep` for sober/flatpak/roblox patterns
   - `kill_process(pid)` — SIGTERM then SIGKILL
   - Watcher support: session, cancel, next_account

5. **`logs.rs`** — Sober log reading for watcher
   - `latest_log_file_for_pid(pid)` — Find log file in Sober log dir
   - `read_log_delta(path, cursor)` — Read new log lines since cursor

6. **`client_settings.rs`** — FPS unlock via Sober config
   - `apply_fps_unlock(max_fps)` — Write to Sober's ClientAppSettings.json
   - `copy_custom_client_settings(path)` — Copy custom settings

### Phase 2: Wire Into Existing Command Layer
Modify these files to add Linux branches:

1. **`src-tauri/src/platform/mod.rs`** — Add `#[cfg(target_os = "linux")] pub mod linux;`

2. **`src-tauri/src/lib.rs`** — Add `cleanup_multi_roblox_on_exit` for Linux

3. **`src-tauri/src/commands/launch.rs`** — Replace all `#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]` stubs with real Linux implementations:
   - `launch_roblox` — Use `platform::linux` to launch Sober
   - `launch_multiple` — Sequential launch with delay
   - `cancel_launch`, `next_account`, `cmd_kill_roblox`, etc.
   - `cmd_enable_multi_roblox`, `cmd_disable_multi_roblox`
   - `cmd_get_roblox_path`, `cmd_apply_fps_unlock`
   - `get_running_instances`, `focus_roblox_window`

4. **`src-tauri/src/commands/watcher.rs`** — Add Linux watcher (log-based, like macOS)

5. **`src-tauri/src/commands/launch_shared.rs`** — Add `get_platform_capabilities` command

### Phase 3: Frontend Adjustments
Minimal changes since frontend already has Linux guards:

1. **`src/utils/platform.ts`** — Add `isLinuxPlatform()` helper
2. **`src/components/settings/GeneralTab.tsx`** — Linux-specific settings (runner selection, multi-rbx toggle)
3. **`src/featureFlags.ts`** — No changes needed (already platform-agnostic)

### Phase 4: Build Configuration
1. **`src-tauri/Cargo.toml`** — No new dependencies needed (all stdlib + existing deps)
2. **`src-tauri/tauri.conf.json`** — Already works for Linux (Tauri v2 supports Linux)
3. **`.github/workflows/`** — Don't touch (just add Linux to CI later)

## Architecture Decisions

- **No Docker**: Use HOME isolation (`env HOME=<dir> flatpak run org.vinegarhq.Sober`). Lighter (~700MB vs 3GB), no privileged mode, no VNC overhead.
- **Wayland-native**: Tauri v2 uses webkit2gtk which supports Wayland. No X11 hacks needed for the app itself. Sober windows are managed by the compositor (Umbriel/wlroots).
- **Window controls limited**: On Wayland (wlroots), there's no reliable way to enumerate/move/resize other app windows (no equivalent of `EnumWindows`). Window arrangement features will be marked as unsupported in capabilities. Focus/minimize might work via `wlr-foreign-toplevel-management-unstable-v1` protocol but that's a future enhancement.
- **Process matching**: Use configurable `CustomProcessMatch` patterns from settings to find Roblox/Sober processes via `pgrep`.

## Security Considerations
- No shell injection: All `Command::new()` calls use proper arg arrays, never `shell=true`
- Instance HOME dirs created with `0o700` permissions
- No cookies written to disk beyond what Sober manages
- Auth cookies passed via Sober config file in isolated HOME, not via command line args
- Flatpak sandbox provides additional isolation

## Files to Create
- `src-tauri/src/platform/linux.rs` (module root)
- `src-tauri/src/platform/linux/core.rs`
- `src-tauri/src/platform/linux/launch.rs`
- `src-tauri/src/platform/linux/multi.rs`
- `src-tauri/src/platform/linux/tracker.rs`
- `src-tauri/src/platform/linux/logs.rs`
- `src-tauri/src/platform/linux/client_settings.rs`

## Files to Modify
- `src-tauri/src/platform/mod.rs` — Add linux module
- `src-tauri/src/lib.rs` — Add Linux cleanup + get_platform_capabilities
- `src-tauri/src/commands/launch.rs` — Replace Linux stubs
- `src-tauri/src/commands/watcher.rs` — Add Linux watcher
- `src/utils/platform.ts` — Add isLinuxPlatform
