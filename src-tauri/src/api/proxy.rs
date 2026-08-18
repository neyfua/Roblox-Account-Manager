use std::sync::RwLock;

use crate::data::settings::SettingsStore;

static CONFIGURED: RwLock<Option<String>> = RwLock::new(None);

pub fn set_proxy(value: Option<String>) {
    if let Ok(mut guard) = CONFIGURED.write() {
        *guard = value;
    }
}

pub fn sync_from_settings(settings: &SettingsStore) {
    let raw = settings.get_string("General", "RobloxHttpProxy");
    set_proxy(if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    });
}

pub fn roblox_client() -> reqwest::Client {
    apply(reqwest::Client::builder()).build().unwrap()
}

pub fn apply(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let raw = CONFIGURED.read().ok().and_then(|g| g.clone());
    let Some(raw) = raw else { return builder };
    let trimmed = raw.trim();

    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("system") || trimmed.eq_ignore_ascii_case("default")
    {
        return builder;
    }

    if trimmed.eq_ignore_ascii_case("none") || trimmed.eq_ignore_ascii_case("direct") {
        return builder.no_proxy();
    }

    let url = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{}", trimmed)
    };

    match reqwest::Proxy::all(&url) {
        Ok(proxy) => builder.proxy(proxy),
        Err(_) => builder,
    }
}