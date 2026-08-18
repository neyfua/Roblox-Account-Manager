use reqwest::header::COOKIE;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

const BATCH_WINDOW_MS: u64 = 50;
const MAX_BATCH_SIZE: usize = 100;
#[allow(dead_code)]
const MAX_PLACE_BATCH_SIZE: usize = 50;

fn cookie_header(security_token: &str) -> String {
    format!(".ROBLOSECURITY={}", security_token)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedThumbnail {
    #[serde(rename = "targetId")]
    pub target_id: i64,
    #[serde(rename = "imageUrl")]
    pub image_url: Option<String>,
    #[serde(rename = "thumbnailType")]
    pub thumbnail_type: String,
}

struct PendingRequest {
    target_id: i64,
    thumbnail_type: String,
    size: String,
    format: String,
    sender: oneshot::Sender<Option<String>>,
}

#[allow(dead_code)]
struct PendingPlaceRequest {
    place_id: i64,
    sender: oneshot::Sender<Option<i64>>,
}

pub struct ImageCache {
    thumbnail_queue: Arc<Mutex<Vec<PendingRequest>>>,
    #[allow(dead_code)]
    place_queue: Arc<Mutex<Vec<PendingPlaceRequest>>>,
    cache: Arc<Mutex<HashMap<String, String>>>,
    place_universe_cache: Arc<Mutex<HashMap<i64, i64>>>,
    batch_active: Arc<Mutex<bool>>,
    #[allow(dead_code)]
    place_batch_active: Arc<Mutex<bool>>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self {
            thumbnail_queue: Arc::new(Mutex::new(Vec::new())),
            place_queue: Arc::new(Mutex::new(Vec::new())),
            cache: Arc::new(Mutex::new(HashMap::new())),
            place_universe_cache: Arc::new(Mutex::new(HashMap::new())),
            batch_active: Arc::new(Mutex::new(false)),
            place_batch_active: Arc::new(Mutex::new(false)),
        }
    }

    fn cache_key(target_id: i64, thumbnail_type: &str, size: &str) -> String {
        format!("{}:{}:{}", target_id, thumbnail_type, size)
    }

    pub async fn get_image(
        &self,
        target_id: i64,
        thumbnail_type: &str,
        size: &str,
        format: &str,
    ) -> Option<String> {
        let key = Self::cache_key(target_id, thumbnail_type, size);

        {
            let cache = self.cache.lock().await;
            if let Some(url) = cache.get(&key) {
                return Some(url.clone());
            }
        }

        let (tx, rx) = oneshot::channel();

        {
            let mut queue = self.thumbnail_queue.lock().await;
            queue.push(PendingRequest {
                target_id,
                thumbnail_type: thumbnail_type.to_string(),
                size: size.to_string(),
                format: format.to_string(),
                sender: tx,
            });
        }

        self.ensure_batch_running().await;
        rx.await.ok().flatten()
    }

    pub async fn get_game_icon(
        &self,
        place_id: i64,
        security_token: Option<&str>,
    ) -> Option<String> {
        let key = Self::cache_key(place_id, "GameIcon", "512x512");

        {
            let cache = self.cache.lock().await;
            if let Some(url) = cache.get(&key) {
                return Some(url.clone());
            }
        }

        let universe_id = {
            let pu_cache = self.place_universe_cache.lock().await;
            pu_cache.get(&place_id).copied()
        };

        let universe_id = match universe_id {
            Some(id) => id,
            None => {
                let resolved = self
                    .resolve_place_to_universe(place_id, security_token)
                    .await;
                match resolved {
                    Some(id) => id,
                    None => {
                        if let Ok(url) = get_asset_image_fallback(place_id, security_token).await {
                            let mut cache = self.cache.lock().await;
                            cache.insert(key, url.clone());
                            return Some(url);
                        }
                        return None;
                    }
                }
            }
        };

        let url = self
            .get_image(universe_id, "GameIcon", "512x512", "png")
            .await;
        if let Some(ref u) = url {
            let mut cache = self.cache.lock().await;
            cache.insert(key, u.clone());
        }
        url
    }

    async fn resolve_place_to_universe(
        &self,
        place_id: i64,
        security_token: Option<&str>,
    ) -> Option<i64> {
        let client = crate::api::proxy::roblox_client();
        let url = format!(
            "https://games.roblox.com/v1/games/multiget-place-details?placeIds={}",
            place_id
        );

        let mut request = client.get(&url);
        if let Some(token) = security_token {
            request = request.header(COOKIE, cookie_header(token));
        }

        let response = request.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }

        let body: serde_json::Value = response.json().await.ok()?;
        let universe_id = body.as_array()?.first()?.get("universeId")?.as_i64()?;

        {
            let mut pu_cache = self.place_universe_cache.lock().await;
            pu_cache.insert(place_id, universe_id);
        }

        Some(universe_id)
    }

    async fn ensure_batch_running(&self) {
        let mut active = self.batch_active.lock().await;
        if *active {
            return;
        }
        *active = true;

        let queue = self.thumbnail_queue.clone();
        let cache = self.cache.clone();
        let batch_active = self.batch_active.clone();

        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(BATCH_WINDOW_MS)).await;

            loop {
                let pending: Vec<PendingRequest> = {
                    let mut q = queue.lock().await;
                    q.drain(..).collect()
                };

                if pending.is_empty() {
                    break;
                }

                let mut requests_by_key: HashMap<String, Vec<PendingRequest>> = HashMap::new();
                for req in pending {
                    let key = format!("{}:{}:{}", req.target_id, req.thumbnail_type, req.size);
                    requests_by_key.entry(key).or_default().push(req);
                }

                let unique_requests: Vec<(i64, String, String, String)> = requests_by_key
                    .keys()
                    .map(|k| {
                        let first = &requests_by_key[k][0];
                        (
                            first.target_id,
                            first.thumbnail_type.clone(),
                            first.size.clone(),
                            first.format.clone(),
                        )
                    })
                    .collect();

                let client = crate::api::proxy::roblox_client();

                for chunk in unique_requests.chunks(MAX_BATCH_SIZE) {
                    let batch_body: Vec<serde_json::Value> = chunk
                        .iter()
                        .map(|(target_id, thumbnail_type, size, format)| {
                            serde_json::json!({
                                "requestId": format!("{}:undefined:{}:{}:{}:regular",
                                    target_id, thumbnail_type, size, format),
                                "type": thumbnail_type,
                                "targetId": target_id,
                                "size": size,
                                "format": format
                            })
                        })
                        .collect();

                    let response = client
                        .post("https://thumbnails.roblox.com/v1/batch")
                        .json(&batch_body)
                        .send()
                        .await;

                    if let Ok(resp) = response {
                        if resp.status().is_success() {
                            if let Ok(body) = resp.json::<serde_json::Value>().await {
                                if let Some(data) = body["data"].as_array() {
                                    let mut c = cache.lock().await;
                                    for item in data {
                                        let target_id = item["targetId"].as_i64().unwrap_or(0);
                                        let image_url =
                                            item["imageUrl"].as_str().unwrap_or_default();
                                        let error_code = item["errorCode"].as_i64().unwrap_or(-1);

                                        if error_code == 0 && !image_url.is_empty() {
                                            let req_id =
                                                item["requestId"].as_str().unwrap_or_default();
                                            let parts: Vec<&str> = req_id.split(':').collect();
                                            if parts.len() >= 4 {
                                                let key = format!(
                                                    "{}:{}:{}",
                                                    target_id, parts[2], parts[3]
                                                );
                                                c.insert(key, image_url.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                {
                    let c = cache.lock().await;
                    for (key, senders) in requests_by_key {
                        let url = c.get(&key).cloned();
                        for req in senders {
                            let _ = req.sender.send(url.clone());
                        }
                    }
                }

                let q = queue.lock().await;
                if q.is_empty() {
                    break;
                }
                drop(q);
            }

            let mut active = batch_active.lock().await;
            *active = false;
        });
    }

    #[allow(dead_code)]
    pub async fn get_cached_thumbnails(&self) -> Vec<CachedThumbnail> {
        let cache = self.cache.lock().await;
        cache
            .iter()
            .map(|(key, url)| {
                let parts: Vec<&str> = key.splitn(3, ':').collect();
                CachedThumbnail {
                    target_id: parts.first().and_then(|s| s.parse().ok()).unwrap_or(0),
                    thumbnail_type: parts.get(1).unwrap_or(&"").to_string(),
                    image_url: Some(url.clone()),
                }
            })
            .collect()
    }

    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        cache.clear();
        let mut pu_cache = self.place_universe_cache.lock().await;
        pu_cache.clear();
    }

    pub async fn get_cached_url(
        &self,
        target_id: i64,
        thumbnail_type: &str,
        size: &str,
    ) -> Option<String> {
        let key = Self::cache_key(target_id, thumbnail_type, size);
        let cache = self.cache.lock().await;
        cache.get(&key).cloned()
    }

    pub async fn get_images_batch(
        &self,
        requests: Vec<(i64, String, String, String)>,
    ) -> Vec<(i64, Option<String>)> {
        let mut receivers = Vec::new();

        {
            let mut queue = self.thumbnail_queue.lock().await;
            let cache = self.cache.lock().await;

            for (target_id, thumbnail_type, size, format) in &requests {
                let key = Self::cache_key(*target_id, thumbnail_type, size);
                if let Some(url) = cache.get(&key) {
                    receivers.push((*target_id, None, Some(url.clone())));
                } else {
                    let (tx, rx) = oneshot::channel();
                    queue.push(PendingRequest {
                        target_id: *target_id,
                        thumbnail_type: thumbnail_type.clone(),
                        size: size.clone(),
                        format: format.clone(),
                        sender: tx,
                    });
                    receivers.push((*target_id, Some(rx), None));
                }
            }
        }

        self.ensure_batch_running().await;

        let mut results = Vec::new();
        for (target_id, rx, cached) in receivers {
            if let Some(url) = cached {
                results.push((target_id, Some(url)));
            } else if let Some(rx) = rx {
                let url = rx.await.ok().flatten();
                results.push((target_id, url));
            }
        }

        results
    }
}

async fn get_asset_image_fallback(
    asset_id: i64,
    security_token: Option<&str>,
) -> Result<String, String> {
    let client = crate::api::proxy::roblox_client();

    let mut request = client.get(format!(
        "https://thumbnails.roblox.com/v1/assets?assetIds={}&returnPolicy=PlaceHolder&size=150x150&format=Png&isCircular=false",
        asset_id
    ));

    if let Some(token) = security_token {
        request = request.header(COOKIE, cookie_header(token));
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err("Asset thumbnail request failed".to_string());
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Parse failed: {}", e))?;

    body["data"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|item| item["imageUrl"].as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "No image URL in response".to_string())
}
