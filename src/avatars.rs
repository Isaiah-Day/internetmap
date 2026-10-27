pub const AVATAR_SIZE: u32 = 64;
pub const PROFILE_BATCH: usize = 25;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{AVATAR_SIZE, PROFILE_BATCH};
    use crate::bsky::PUBLIC_ENDPOINT;
    use egui::ColorImage;
    use reqwest::blocking::Client;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const URL_CACHE_PATH: &str = "avatars.json";
    const IMAGE_CACHE_DIR: &str = "avatar_cache";
    const IMAGE_THREADS: usize = 8;

    /// Loads avatars for the given DIDs.
    ///
    /// Profile URLs are cached in `avatars.json`; image bytes are cached in
    /// `avatar_cache/`. On the first run this will fetch the missing profiles and
    /// images. Later runs are just disk reads.
    pub fn load_avatars(dids: &[String]) -> HashMap<String, Arc<ColorImage>> {
        let urls = load_or_fetch_avatar_urls(dids);
        if urls.is_empty() {
            return HashMap::new();
        }

        let _ = fs::create_dir_all(IMAGE_CACHE_DIR);

        let jobs: Vec<(String, String)> = dids
            .iter()
            .filter_map(|did| urls.get(did).map(|url| (did.clone(), url.clone())))
            .collect();
        if jobs.is_empty() {
            return HashMap::new();
        }

        let results: Mutex<HashMap<String, Arc<ColorImage>>> = Mutex::new(HashMap::new());
        let chunk_size = jobs.len().div_ceil(IMAGE_THREADS).max(1);

        std::thread::scope(|scope| {
            for chunk in jobs.chunks(chunk_size) {
                let results = &results;
                scope.spawn(move || {
                    let client = match Client::builder().timeout(Duration::from_secs(20)).build() {
                        Ok(client) => client,
                        Err(_) => return,
                    };

                    for (did, url) in chunk {
                        let cache_path = Path::new(IMAGE_CACHE_DIR).join(format!("{did}.img"));
                        let bytes = fs::read(&cache_path).ok().or_else(|| {
                            let bytes = client
                                .get(url)
                                .send()
                                .ok()?
                                .error_for_status()
                                .ok()?
                                .bytes()
                                .ok()?;
                            let bytes = bytes.to_vec();
                            let _ = fs::write(&cache_path, &bytes);
                            Some(bytes)
                        });

                        let Some(bytes) = bytes else {
                            continue;
                        };
                        let Ok(image) = image::load_from_memory(&bytes) else {
                            continue;
                        };
                        let image = image
                            .resize(
                                AVATAR_SIZE,
                                AVATAR_SIZE,
                                image::imageops::FilterType::Triangle,
                            )
                            .to_rgba8();
                        let color_image = ColorImage::from_rgba_unmultiplied(
                            [image.width() as usize, image.height() as usize],
                            image.as_raw(),
                        );

                        results
                            .lock()
                            .unwrap()
                            .insert(did.clone(), Arc::new(color_image));
                    }
                });
            }
        });

        results.into_inner().unwrap()
    }

    fn load_or_fetch_avatar_urls(dids: &[String]) -> HashMap<String, String> {
        let mut urls: HashMap<String, String> = fs::read_to_string(URL_CACHE_PATH)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

        let missing: Vec<&String> = dids.iter().filter(|did| !urls.contains_key(*did)).collect();
        if missing.is_empty() {
            return urls;
        }

        let client = match Client::builder().timeout(Duration::from_secs(20)).build() {
            Ok(client) => client,
            Err(_) => return urls,
        };

        for chunk in missing.chunks(PROFILE_BATCH) {
            let actors: Vec<(&str, &str)> =
                chunk.iter().map(|did| ("actors", did.as_str())).collect();

            let response = client
                .get(format!("{PUBLIC_ENDPOINT}/xrpc/app.bsky.actor.getProfiles"))
                .query(&actors)
                .send()
                .and_then(|response| response.error_for_status())
                .and_then(|response| response.json::<Value>());

            let Ok(value) = response else {
                continue;
            };
            let Some(profiles) = value["profiles"].as_array() else {
                continue;
            };

            for profile in profiles {
                let (Some(did), Some(avatar)) =
                    (profile["did"].as_str(), profile["avatar"].as_str())
                else {
                    continue;
                };
                urls.insert(did.to_owned(), avatar.to_owned());
            }
        }

        if let Ok(text) = serde_json::to_string_pretty(&urls) {
            let _ = fs::write(URL_CACHE_PATH, text);
        }

        urls
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::{AVATAR_SIZE, PROFILE_BATCH};
    use crate::PUBLIC_ENDPOINT;
    use egui::ColorImage;
    use futures_util::StreamExt;
    use js_sys::{Uint8Array, encode_uri_component};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::Arc;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::Response;

    const IMAGE_CONCURRENCY: usize = 8;

    pub async fn load_avatars(dids: &[String]) -> HashMap<String, Arc<ColorImage>> {
        let urls = load_avatar_urls(dids).await;
        if urls.is_empty() {
            return HashMap::new();
        }

        let jobs = urls.into_iter().map(|(did, url)| async move {
            let bytes = fetch_bytes(&url).await?;
            let image = image::load_from_memory(&bytes).ok()?;
            let image = image
                .resize(
                    AVATAR_SIZE,
                    AVATAR_SIZE,
                    image::imageops::FilterType::Triangle,
                )
                .to_rgba8();
            let color_image = ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            );
            Some((did, Arc::new(color_image)))
        });

        futures_util::stream::iter(jobs)
            .buffer_unordered(IMAGE_CONCURRENCY)
            .filter_map(|result| async move { result })
            .collect()
            .await
    }

    async fn load_avatar_urls(dids: &[String]) -> HashMap<String, String> {
        let mut urls = HashMap::new();

        for chunk in dids.chunks(PROFILE_BATCH) {
            let mut url = format!("{PUBLIC_ENDPOINT}/xrpc/app.bsky.actor.getProfiles?");
            for (i, did) in chunk.iter().enumerate() {
                if i > 0 {
                    url.push('&');
                }
                url.push_str("actors=");
                if let Some(encoded) = encode_uri_component(did).as_string() {
                    url.push_str(&encoded);
                }
            }

            let Some(text) = fetch_text(&url).await else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(profiles) = value["profiles"].as_array() else {
                continue;
            };
            for profile in profiles {
                let (Some(did), Some(avatar)) =
                    (profile["did"].as_str(), profile["avatar"].as_str())
                else {
                    continue;
                };
                urls.insert(did.to_owned(), avatar.to_owned());
            }
        }

        urls
    }

    async fn fetch_text(url: &str) -> Option<String> {
        let response = fetch(url).await?;
        let text = JsFuture::from(response.text().ok()?).await.ok()?;
        text.as_string()
    }

    async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
        // cdn.bsky.app does not send CORS headers, so browsers cannot fetch
        // avatar bytes directly. Route the public avatar URL through an
        // image proxy that does support CORS and resizing.
        let encoded = encode_uri_component(url).as_string()?;
        let proxy_url = format!(
            "https://images.weserv.nl/?url={encoded}&w={AVATAR_SIZE}&h={AVATAR_SIZE}&fit=cover&output=webp"
        );

        let response = fetch(&proxy_url).await?;
        let buffer = JsFuture::from(response.array_buffer().ok()?).await.ok()?;
        Some(Uint8Array::new(&buffer).to_vec())
    }

    async fn fetch(url: &str) -> Option<Response> {
        let window = web_sys::window()?;
        let response = JsFuture::from(window.fetch_with_str(url)).await.ok()?;
        let response: Response = response.dyn_into().ok()?;
        response.ok().then_some(response)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::load_avatars;
#[cfg(target_arch = "wasm32")]
pub use web::load_avatars;
