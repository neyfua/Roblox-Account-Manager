use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct ScriptPermissions {
    pub allow_invoke: bool,
    pub allow_http: bool,
    #[serde(rename = "allowWebSocket", alias = "allowWebsocket")]
    pub allow_websocket: bool,
    pub allow_window: bool,
    pub allow_modal: bool,
    pub allow_settings: bool,
    pub allow_ui: bool,
}

impl Default for ScriptPermissions {
    fn default() -> Self {
        Self {
            allow_invoke: false,
            allow_http: false,
            allow_websocket: false,
            allow_window: false,
            allow_modal: false,
            allow_settings: false,
            allow_ui: false,
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn default_language() -> String {
    "javascript".to_string()
}

fn default_enabled() -> bool {
    true
}

const MAX_SCRIPTS_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCRIPT_COUNT: usize = 256;
const MAX_SCRIPT_ID_CHARS: usize = 96;
const MAX_SCRIPT_NAME_CHARS: usize = 120;
const MAX_SCRIPT_DESCRIPTION_CHARS: usize = 2048;
const MAX_SCRIPT_LANGUAGE_CHARS: usize = 32;
const MAX_SCRIPT_SOURCE_BYTES: usize = 262_144;

fn contains_disallowed_control_chars(value: &str) -> bool {
    value
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t')
}

fn validate_script_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Script id is required".to_string());
    }
    if id.len() > MAX_SCRIPT_ID_CHARS {
        return Err(format!(
            "Script id exceeds {} characters",
            MAX_SCRIPT_ID_CHARS
        ));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        return Err(
            "Script id contains unsupported characters (allowed: a-z, A-Z, 0-9, -, _, .)"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_script_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Script name is required".to_string());
    }
    if name.chars().count() > MAX_SCRIPT_NAME_CHARS {
        return Err(format!(
            "Script name exceeds {} characters",
            MAX_SCRIPT_NAME_CHARS
        ));
    }
    if contains_disallowed_control_chars(name) {
        return Err("Script name contains unsupported control characters".to_string());
    }
    Ok(())
}

fn validate_script_description(description: &str) -> Result<(), String> {
    if description.chars().count() > MAX_SCRIPT_DESCRIPTION_CHARS {
        return Err(format!(
            "Script description exceeds {} characters",
            MAX_SCRIPT_DESCRIPTION_CHARS
        ));
    }
    if contains_disallowed_control_chars(description) {
        return Err("Script description contains unsupported control characters".to_string());
    }
    Ok(())
}

fn normalize_script_language(language: &str) -> String {
    let lowered = language.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return default_language();
    }
    if lowered == "js" || lowered == "javascript" {
        return "javascript".to_string();
    }
    "javascript".to_string()
}

fn validate_script_source(source: &str) -> Result<(), String> {
    if source.len() > MAX_SCRIPT_SOURCE_BYTES {
        return Err(format!(
            "Script source exceeds {} bytes",
            MAX_SCRIPT_SOURCE_BYTES
        ));
    }
    if source.contains('\0') {
        return Err("Script source contains null bytes".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedScript {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub source: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub trusted: bool,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub permissions: ScriptPermissions,
    #[serde(default = "now_ms")]
    pub created_at_ms: i64,
    #[serde(default = "now_ms")]
    pub updated_at_ms: i64,
}

pub struct ScriptStore {
    scripts: Mutex<Vec<ManagedScript>>,
    file_path: PathBuf,
}

impl ScriptStore {
    pub fn new(file_path: PathBuf) -> Self {
        let store = Self {
            scripts: Mutex::new(Vec::new()),
            file_path,
        };
        let _ = store.load_from_disk();
        store
    }

    fn load_from_disk(&self) -> Result<(), String> {
        if !self.file_path.exists() {
            return Ok(());
        }

        let metadata = fs::metadata(&self.file_path)
            .map_err(|e| format!("Failed to read scripts file metadata: {}", e))?;
        if metadata.len() > MAX_SCRIPTS_FILE_BYTES {
            return Err(format!(
                "Scripts file is too large (max {} bytes)",
                MAX_SCRIPTS_FILE_BYTES
            ));
        }

        let data =
            fs::read(&self.file_path).map_err(|e| format!("Failed to read scripts file: {}", e))?;
        if data.is_empty() {
            return Ok(());
        }

        let parsed = serde_json::from_slice::<Vec<ManagedScript>>(&data)
            .map_err(|e| format!("Failed to parse scripts file: {}", e))?;

        let mut scripts = self.scripts.lock().map_err(|e| e.to_string())?;
        *scripts = parsed;
        Ok(())
    }

    fn save_to_disk(&self) -> Result<(), String> {
        let scripts = self.scripts.lock().map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(&*scripts)
            .map_err(|e| format!("Failed to serialize scripts: {}", e))?;
        fs::write(&self.file_path, bytes)
            .map_err(|e| format!("Failed to write scripts file: {}", e))?;
        Ok(())
    }

    pub fn get_all(&self) -> Result<Vec<ManagedScript>, String> {
        let mut scripts = self.scripts.lock().map_err(|e| e.to_string())?.clone();
        scripts.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(scripts)
    }

    pub fn upsert(&self, mut script: ManagedScript) -> Result<ManagedScript, String> {
        let id = script.id.trim();
        validate_script_id(id)?;
        script.id = id.to_string();

        let name = script.name.trim();
        validate_script_name(name)?;
        script.name = name.to_string();

        script.description = script.description.trim().to_string();
        validate_script_description(&script.description)?;

        if script.language.chars().count() > MAX_SCRIPT_LANGUAGE_CHARS {
            return Err(format!(
                "Script language exceeds {} characters",
                MAX_SCRIPT_LANGUAGE_CHARS
            ));
        }
        script.language = normalize_script_language(&script.language);

        validate_script_source(&script.source)?;

        let mut scripts = self.scripts.lock().map_err(|e| e.to_string())?;
        let now = now_ms();

        if let Some(existing) = scripts.iter_mut().find(|s| s.id == script.id) {
            let created = existing.created_at_ms;
            *existing = script;
            existing.created_at_ms = created;
            existing.updated_at_ms = now;
            let out = existing.clone();
            drop(scripts);
            self.save_to_disk()?;
            return Ok(out);
        }

        if scripts.len() >= MAX_SCRIPT_COUNT {
            return Err(format!("Script limit reached (max {})", MAX_SCRIPT_COUNT));
        }

        script.created_at_ms = now;
        script.updated_at_ms = now;
        scripts.push(script.clone());
        drop(scripts);
        self.save_to_disk()?;
        Ok(script)
    }

    pub fn remove(&self, script_id: &str) -> Result<bool, String> {
        let normalized_id = script_id.trim();
        if normalized_id.is_empty() {
            return Err("Script id is required".to_string());
        }

        let mut scripts = self.scripts.lock().map_err(|e| e.to_string())?;
        let before = scripts.len();
        scripts.retain(|s| s.id != normalized_id);
        let removed = scripts.len() < before;
        drop(scripts);
        if removed {
            self.save_to_disk()?;
        }
        Ok(removed)
    }
}

pub fn get_scripts_path() -> PathBuf {
    crate::data::settings::get_settings_path().with_file_name("RAMScripts.json")
}

#[tauri::command]
pub fn get_scripts(state: tauri::State<'_, ScriptStore>) -> Result<Vec<ManagedScript>, String> {
    state.get_all()
}

#[tauri::command]
pub fn save_script(
    state: tauri::State<'_, ScriptStore>,
    script: ManagedScript,
) -> Result<ManagedScript, String> {
    state.upsert(script)
}

#[tauri::command]
pub fn delete_script(
    state: tauri::State<'_, ScriptStore>,
    script_id: String,
) -> Result<bool, String> {
    state.remove(&script_id)
}
