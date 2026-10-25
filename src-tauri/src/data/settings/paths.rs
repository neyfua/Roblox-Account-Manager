fn get_runtime_data_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local/share")
            });
        let dir = base.join("roblox-account-manager");
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    #[cfg(not(target_os = "linux"))]
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

pub fn get_settings_path() -> PathBuf {
    get_runtime_data_dir().join("RAMSettings.ini")
}

pub fn get_theme_path() -> PathBuf {
    get_runtime_data_dir().join("RAMTheme.ini")
}

pub fn get_theme_presets_path() -> PathBuf {
    get_runtime_data_dir().join("RAMThemePresets.json")
}

pub fn get_theme_fonts_dir() -> PathBuf {
    get_runtime_data_dir().join("RAMThemeFonts")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeFontAssetImportResult {
    pub file: String,
    pub suggested_family: String,
}

fn sanitize_font_family_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Custom Font")
        .trim();
    if stem.is_empty() {
        return "Custom Font".to_string();
    }
    stem.to_string()
}

fn is_allowed_font_ext(ext: &str) -> bool {
    matches!(ext, "ttf" | "otf" | "woff" | "woff2")
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

