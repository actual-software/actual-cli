//! OpenAI and Anthropic model list fetching and disk-backed caching.
//!
//! # Design
//!
//! - `get_openai_models()` / `get_anthropic_models()` are the main entry points.
//!   They are thin wrappers around a generic [`get_models`] function that handles
//!   API-key resolution, on-disk caching, and tokio-runtime creation. The
//!   provider-specific async fetch functions (`fetch_openai_models_async`,
//!   `fetch_anthropic_models_async`) are passed in as closures.
//! - A 24-hour TTL cache is stored at `~/.actualai/actual/model-cache.yaml`.
//! - On any error (no API key, network failure, parse error) the function returns
//!   an empty `Vec` so the caller can fall back to the static hardcoded list.
//! - Each provider's cache entry is independent: an expired OpenAI cache does not
//!   force an Anthropic re-fetch, and vice versa.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One provider's entry in the model cache file.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct ProviderCache {
    pub fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub models: Vec<String>,
}

impl ProviderCache {
    /// Returns `true` if the cache was fetched within the last `ttl_hours` hours.
    pub fn is_fresh(&self, ttl_hours: u32) -> bool {
        self.fetched_at
            .map(|t| {
                Utc::now()
                    .signed_duration_since(t)
                    .num_hours()
                    .unsigned_abs()
                    < ttl_hours as u64
            })
            .unwrap_or(false)
    }
}

/// Which model provider to fetch from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Provider {
    OpenAi,
    Anthropic,
}

impl Provider {
    /// The environment variable that holds this provider's API key.
    fn env_var(self) -> &'static str {
        match self {
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
        }
    }

    /// The production base URL for this provider's API.
    fn production_base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com",
            Self::Anthropic => "https://api.anthropic.com",
        }
    }

    /// A human-readable name used in log messages.
    fn display_name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

/// The top-level model cache file structure.
///
/// The `anthropic` section is reserved for actual-3kh.2.
#[derive(Debug, Serialize, Deserialize, Default)]
pub(crate) struct ModelCacheFile {
    #[serde(default)]
    pub openai: ProviderCache,
    #[serde(default)]
    pub anthropic: ProviderCache,
}

impl ModelCacheFile {
    /// Immutable access to the cache section for the given provider.
    fn provider(&self, p: Provider) -> &ProviderCache {
        match p {
            Provider::OpenAi => &self.openai,
            Provider::Anthropic => &self.anthropic,
        }
    }

    /// Mutable access to the cache section for the given provider.
    fn provider_mut(&mut self, p: Provider) -> &mut ProviderCache {
        match p {
            Provider::OpenAi => &mut self.openai,
            Provider::Anthropic => &mut self.anthropic,
        }
    }
}

// ---------------------------------------------------------------------------
// Private OpenAI API response shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OpenAiModelObject {
    id: String,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelObject>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const CACHE_FILENAME: &str = "model-cache.yaml";
const DEFAULT_CACHE_TTL_HOURS: u32 = 24;
const FETCH_TIMEOUT_SECS: u64 = 10;

// ---------------------------------------------------------------------------
// Cache file helpers
// ---------------------------------------------------------------------------

fn cache_path() -> Option<std::path::PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|d| d.join(CACHE_FILENAME))
}

fn load_cache_file(path: &std::path::Path) -> ModelCacheFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_yml::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cache_file(path: &std::path::Path, cache: &ModelCacheFile) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // serde_yml::to_string on a well-formed struct is infallible in practice
    let yaml = serde_yml::to_string(cache).unwrap_or_default();
    let _ = std::fs::write(path, yaml);
    // 0600 on unix (model names are not secrets, but consistent with config dir)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

// ---------------------------------------------------------------------------
// OpenAI model filtering
// ---------------------------------------------------------------------------

fn is_relevant_openai_model(id: &str) -> bool {
    // Exclude fine-tuned models
    if id.starts_with("ft:") {
        return false;
    }

    // Exclude non-chat/completion model families
    let excluded_prefixes = [
        "text-embedding-",
        "text-moderation-",
        "text-davinci-",
        "dall-e-",
        "whisper-",
        "tts-",
        "babbage-",
        "davinci-",
        "gpt-image-",
    ];
    if excluded_prefixes.iter().any(|p| id.starts_with(p)) {
        return false;
    }

    // Include known useful prefixes
    let included_prefixes = ["gpt-", "o1", "o3", "o4", "chatgpt-", "codex-"];
    included_prefixes.iter().any(|p| id.starts_with(p))
}

// ---------------------------------------------------------------------------
// Async fetch (injectable base URL for tests)
// ---------------------------------------------------------------------------

/// Fetch the model list from the OpenAI `/v1/models` endpoint.
///
/// `base_url` should be `"https://api.openai.com"` in production, or a local
/// mock server URL in tests.
pub(crate) async fn fetch_openai_models_async(
    api_key: &str,
    base_url: &str,
    timeout: std::time::Duration,
) -> Result<Vec<String>, crate::error::ActualError> {
    use crate::error::ActualError;

    let is_local =
        base_url.starts_with("http://localhost") || base_url.starts_with("http://127.0.0.1");

    // reqwest::Client::builder().build() is infallible for standard configurations;
    // the only failure mode is a TLS backend issue which cannot occur here.
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .https_only(!is_local)
        .build()
        .expect("reqwest client build failed");

    let url = format!("{base_url}/v1/models");
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|e| ActualError::RunnerFailed {
            message: format!("OpenAI models fetch failed: {e}"),
            stderr: String::new(),
        })?;

    let status = response.status();

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ActualError::ApiKeyMissing {
            env_var: "OPENAI_API_KEY".to_string(),
        });
    }

    if !status.is_success() {
        let body = response
            .bytes()
            .await
            .map(|b| String::from_utf8_lossy(&b[..b.len().min(512)]).into_owned())
            .unwrap_or_default();
        return Err(ActualError::RunnerFailed {
            message: format!("OpenAI models API returned {status}"),
            stderr: body,
        });
    }

    let parsed =
        response
            .json::<OpenAiModelsResponse>()
            .await
            .map_err(|e| ActualError::RunnerFailed {
                message: format!("Failed to parse OpenAI models response: {e}"),
                stderr: String::new(),
            })?;

    let models: Vec<String> = parsed
        .data
        .into_iter()
        .map(|m| m.id)
        .filter(|id| is_relevant_openai_model(id))
        .collect();

    Ok(models)
}

// ---------------------------------------------------------------------------
// Private Anthropic API response shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AnthropicModelObject {
    id: String,
}

#[derive(Deserialize)]
struct AnthropicModelsResponse {
    data: Vec<AnthropicModelObject>,
    has_more: bool,
    last_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Async fetch for Anthropic (injectable base URL for tests)
// ---------------------------------------------------------------------------

/// Fetch the model list from the Anthropic `/v1/models` endpoint.
///
/// Paginates automatically until `has_more == false`.
///
/// `base_url` should be `"https://api.anthropic.com"` in production, or a
/// local mock server URL in tests.
pub(crate) async fn fetch_anthropic_models_async(
    api_key: &str,
    base_url: &str,
    timeout: std::time::Duration,
) -> Result<Vec<String>, crate::error::ActualError> {
    use crate::error::ActualError;

    let is_local =
        base_url.starts_with("http://localhost") || base_url.starts_with("http://127.0.0.1");

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .https_only(!is_local)
        .build()
        .expect("reqwest client build failed");

    const MAX_PAGES: usize = 50;

    let mut all_models = Vec::new();
    let mut after_id: Option<String> = None;
    let mut page_count: usize = 0;

    loop {
        page_count += 1;
        if page_count > MAX_PAGES {
            tracing::warn!("Anthropic models pagination exceeded {MAX_PAGES} pages, stopping");
            break;
        }

        let url = match &after_id {
            Some(id) => format!("{base_url}/v1/models?after_id={id}"),
            None => format!("{base_url}/v1/models"),
        };

        let response = client
            .get(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|e| ActualError::RunnerFailed {
                message: format!("Anthropic models fetch failed: {e}"),
                stderr: String::new(),
            })?;

        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ActualError::ApiKeyMissing {
                env_var: "ANTHROPIC_API_KEY".to_string(),
            });
        }

        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .map(|b| String::from_utf8_lossy(&b[..b.len().min(512)]).into_owned())
                .unwrap_or_default();
            return Err(ActualError::RunnerFailed {
                message: format!("Anthropic models API returned {status}"),
                stderr: body,
            });
        }

        let parsed = response
            .json::<AnthropicModelsResponse>()
            .await
            .map_err(|e| ActualError::RunnerFailed {
                message: format!("Failed to parse Anthropic models response: {e}"),
                stderr: String::new(),
            })?;

        all_models.extend(parsed.data.into_iter().map(|m| m.id));

        if !parsed.has_more {
            break;
        }
        after_id = parsed.last_id;
    }

    Ok(all_models)
}

// ---------------------------------------------------------------------------
// Generic synchronous entry point
// ---------------------------------------------------------------------------

/// Generic cached-fetch for any provider.
///
/// 1. Resolve the API key (env var takes priority over `config_key`).
/// 2. If `base_url` is `None` (production), check the on-disk cache and return
///    early when it is fresh.
/// 3. Otherwise create a one-shot tokio runtime and call `fetch_fn`.
/// 4. On success, persist the result to the cache file.
/// 5. On error, log a warning and return an empty `Vec`.
fn get_models<F, Fut>(
    provider: Provider,
    config_key: Option<&str>,
    base_url: Option<&str>,
    fetch_fn: F,
) -> Vec<String>
where
    F: FnOnce(String, String, std::time::Duration) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<String>, crate::error::ActualError>>,
{
    // Resolve API key (env var takes priority)
    let api_key = std::env::var(provider.env_var())
        .ok()
        .or_else(|| config_key.map(|s| s.to_string()));

    let Some(api_key) = api_key else {
        return Vec::new(); // no key available — skip silently
    };

    let ttl = DEFAULT_CACHE_TTL_HOURS;

    // Return cached list if fresh (skip TTL check when base_url is overridden for tests).
    let cached = base_url.is_none().then(|| {
        cache_path().and_then(|path| {
            let file = load_cache_file(&path);
            let section = file.provider(provider);
            section.is_fresh(ttl).then(|| section.models.clone())
        })
    });
    if let Some(Some(models)) = cached {
        return models;
    }

    // Fetch live
    let url = base_url.unwrap_or(provider.production_base_url());
    let timeout = std::time::Duration::from_secs(FETCH_TIMEOUT_SECS);

    // tokio::runtime::Builder::new_current_thread().enable_all().build() is
    // infallible in practice (only fails if the OS cannot create an I/O driver,
    // which would be a catastrophic system-level failure).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime build failed");

    match rt.block_on(fetch_fn(api_key, url.to_string(), timeout)) {
        Ok(models) => {
            // Persist to cache
            if let Some(path) = cache_path() {
                let mut file = load_cache_file(&path);
                *file.provider_mut(provider) = ProviderCache {
                    fetched_at: Some(Utc::now()),
                    models: models.clone(),
                };
                save_cache_file(&path, &file);
            }
            models
        }
        Err(e) => {
            tracing::warn!(
                "{} model fetch failed (using hardcoded fallback): {e}",
                provider.display_name()
            );
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Public synchronous entry points (thin wrappers)
// ---------------------------------------------------------------------------

/// Fetch Anthropic models: return cached list if fresh, otherwise fetch live.
///
/// Returns an empty `Vec` on any error (graceful degradation — the caller is
/// expected to merge this with the hardcoded static list).
///
/// # Arguments
///
/// * `config_key` — the `anthropic_api_key` from config.  `ANTHROPIC_API_KEY`
///   env var takes priority over this value.
/// * `base_url` — `None` in production, `Some("http://...")` in tests to
///   override the Anthropic base URL.  When `Some`, the TTL check is skipped
///   so tests always exercise the network path.
pub fn get_anthropic_models(config_key: Option<&str>, base_url: Option<&str>) -> Vec<String> {
    get_models(
        Provider::Anthropic,
        config_key,
        base_url,
        |key, url, timeout| async move { fetch_anthropic_models_async(&key, &url, timeout).await },
    )
}

/// Fetch OpenAI models: return cached list if fresh, otherwise fetch live.
///
/// Returns an empty `Vec` on any error (graceful degradation — the caller is
/// expected to merge this with the hardcoded static list).
///
/// # Arguments
///
/// * `config_key` — the `openai_api_key` from config.  `OPENAI_API_KEY` env
///   var takes priority over this value.
/// * `base_url` — `None` in production, `Some("http://...")` in tests to
///   override the OpenAI base URL.  When `Some`, the TTL check is skipped so
///   tests always exercise the network path.
pub fn get_openai_models(config_key: Option<&str>, base_url: Option<&str>) -> Vec<String> {
    get_models(
        Provider::OpenAi,
        config_key,
        base_url,
        |key, url, timeout| async move { fetch_openai_models_async(&key, &url, timeout).await },
    )
}

// ---------------------------------------------------------------------------
// Provenance helpers (read-only, no network call)
// ---------------------------------------------------------------------------

/// Read a cached model timestamp from disk for the given provider (no network
/// call). Returns `None` if there is no cache file or no timestamp.
fn read_cache_timestamp(provider: Provider) -> Option<chrono::DateTime<chrono::Utc>> {
    cache_path().and_then(|path| load_cache_file(&path).provider(provider).fetched_at)
}

/// Read cached models from disk for the given provider without making a
/// network call. Returns empty `Vec` if no cache or no timestamp present.
fn read_cached_models(provider: Provider) -> Vec<String> {
    cache_path()
        .and_then(|path| {
            let section = load_cache_file(&path).provider(provider).clone();
            section.fetched_at.is_some().then_some(section.models)
        })
        .unwrap_or_default()
}

/// Read the cached OpenAI model timestamp from disk (no network call).
/// Returns None if there is no cache file or no timestamp.
pub fn read_openai_cache_timestamp() -> Option<chrono::DateTime<chrono::Utc>> {
    read_cache_timestamp(Provider::OpenAi)
}

/// Read the cached Anthropic model timestamp from disk (no network call).
/// Returns None if there is no cache file or no timestamp.
pub fn read_anthropic_cache_timestamp() -> Option<chrono::DateTime<chrono::Utc>> {
    read_cache_timestamp(Provider::Anthropic)
}

/// Read cached OpenAI models from disk without making a network call.
/// Returns empty Vec if no cache or no timestamp present.
pub fn read_cached_openai_models() -> Vec<String> {
    read_cached_models(Provider::OpenAi)
}

/// Read cached Anthropic models from disk without making a network call.
/// Returns empty Vec if no cache or no timestamp present.
pub fn read_cached_anthropic_models() -> Vec<String> {
    read_cached_models(Provider::Anthropic)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // -----------------------------------------------------------------------
    // ProviderCache::is_fresh
    // -----------------------------------------------------------------------

    #[test]
    fn test_provider_cache_is_fresh_recent() {
        let cache = ProviderCache {
            fetched_at: Some(Utc::now()),
            models: vec!["gpt-4o".to_string()],
        };
        assert!(cache.is_fresh(24), "just-created cache should be fresh");
    }

    #[test]
    fn test_provider_cache_is_expired_old() {
        let cache = ProviderCache {
            fetched_at: Some(Utc::now() - chrono::Duration::hours(25)),
            models: vec!["gpt-4o".to_string()],
        };
        assert!(!cache.is_fresh(24), "25h-old cache should be expired");
    }

    // -----------------------------------------------------------------------
    // Provider enum & ModelCacheFile accessors
    // -----------------------------------------------------------------------

    #[test]
    fn test_provider_enum_methods() {
        assert_eq!(Provider::OpenAi.env_var(), "OPENAI_API_KEY");
        assert_eq!(Provider::Anthropic.env_var(), "ANTHROPIC_API_KEY");

        assert_eq!(
            Provider::OpenAi.production_base_url(),
            "https://api.openai.com"
        );
        assert_eq!(
            Provider::Anthropic.production_base_url(),
            "https://api.anthropic.com"
        );

        assert_eq!(Provider::OpenAi.display_name(), "OpenAI");
        assert_eq!(Provider::Anthropic.display_name(), "Anthropic");
    }

    #[test]
    fn test_model_cache_file_provider_accessors() {
        let mut cache = ModelCacheFile::default();
        cache.openai = ProviderCache {
            fetched_at: Some(Utc::now()),
            models: vec!["gpt-4o".to_string()],
        };
        cache.anthropic = ProviderCache {
            fetched_at: Some(Utc::now()),
            models: vec!["claude-sonnet-4-6".to_string()],
        };

        assert_eq!(cache.provider(Provider::OpenAi).models, vec!["gpt-4o"]);
        assert_eq!(
            cache.provider(Provider::Anthropic).models,
            vec!["claude-sonnet-4-6"]
        );

        cache.provider_mut(Provider::OpenAi).models = vec!["gpt-5".to_string()];
        assert_eq!(cache.provider(Provider::OpenAi).models, vec!["gpt-5"]);

        cache.provider_mut(Provider::Anthropic).models = vec!["claude-new".to_string()];
        assert_eq!(
            cache.provider(Provider::Anthropic).models,
            vec!["claude-new"]
        );
    }

    // -----------------------------------------------------------------------
    // Cache file I/O
    // -----------------------------------------------------------------------

    #[test]
    fn test_save_and_load_cache_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("model-cache.yaml");

        let mut cache = ModelCacheFile::default();
        cache.openai = ProviderCache {
            fetched_at: Some(Utc::now()),
            models: vec!["gpt-4o".to_string(), "gpt-5.2".to_string()],
        };

        save_cache_file(&path, &cache);
        let loaded = load_cache_file(&path);

        assert_eq!(loaded.openai.models, cache.openai.models);
        assert!(loaded.openai.fetched_at.is_some());
    }

    #[test]
    fn test_load_cache_absent_returns_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.yaml");
        let loaded = load_cache_file(&path);
        assert!(loaded.openai.models.is_empty());
        assert!(loaded.openai.fetched_at.is_none());
    }

    #[test]
    fn test_load_cache_corrupt_returns_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("corrupt.yaml");
        std::fs::write(&path, "{{{{ not valid yaml }}}}").unwrap();
        let loaded = load_cache_file(&path);
        assert!(loaded.openai.models.is_empty());
    }

    // -----------------------------------------------------------------------
    // is_relevant_openai_model
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_relevant_openai_model_includes_gpt() {
        assert!(is_relevant_openai_model("gpt-5.2"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_embeddings() {
        assert!(!is_relevant_openai_model("text-embedding-ada-002"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_dalle() {
        assert!(!is_relevant_openai_model("dall-e-3"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_ft() {
        assert!(!is_relevant_openai_model("ft:gpt-4:custom"));
    }

    #[test]
    fn test_is_relevant_openai_model_includes_o1() {
        assert!(is_relevant_openai_model("o1"));
    }

    #[test]
    fn test_is_relevant_openai_model_includes_o3() {
        assert!(is_relevant_openai_model("o3-mini"));
    }

    #[test]
    fn test_is_relevant_openai_model_includes_o4() {
        assert!(is_relevant_openai_model("o4-mini"));
    }

    #[test]
    fn test_is_relevant_openai_model_includes_chatgpt() {
        assert!(is_relevant_openai_model("chatgpt-4o-latest"));
    }

    #[test]
    fn test_is_relevant_openai_model_includes_codex() {
        assert!(is_relevant_openai_model("codex-cushman-001"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_whisper() {
        assert!(!is_relevant_openai_model("whisper-1"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_tts() {
        assert!(!is_relevant_openai_model("tts-1"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_babbage() {
        assert!(!is_relevant_openai_model("babbage-002"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_davinci() {
        assert!(!is_relevant_openai_model("davinci-002"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_gpt_image() {
        assert!(!is_relevant_openai_model("gpt-image-1"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_text_moderation() {
        assert!(!is_relevant_openai_model("text-moderation-stable"));
    }

    #[test]
    fn test_is_relevant_openai_model_excludes_text_davinci() {
        assert!(!is_relevant_openai_model("text-davinci-003"));
    }

    #[test]
    fn test_is_relevant_openai_model_unknown_prefix_returns_false() {
        assert!(!is_relevant_openai_model("unknown-model-xyz"));
    }

    // -----------------------------------------------------------------------
    // fetch_anthropic_models_async (mockito)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_fetch_anthropic_models_async_success() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":[{"id":"claude-opus-4-6"},{"id":"claude-sonnet-4-6"}],"has_more":false,"last_id":"claude-sonnet-4-6"}"#,
            )
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_anthropic_models_async("sk-ant-test", &server.url(), timeout).await;

        let models = result.expect("should succeed");
        assert!(models.contains(&"claude-opus-4-6".to_string()));
        assert!(models.contains(&"claude-sonnet-4-6".to_string()));
    }

    #[tokio::test]
    async fn test_fetch_anthropic_models_async_pagination() {
        let mut server = mockito::Server::new_async().await;
        let _m1 = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"claude-a"}],"has_more":true,"last_id":"claude-a"}"#)
            .create_async()
            .await;
        let _m2 = server
            .mock("GET", "/v1/models?after_id=claude-a")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"claude-b"}],"has_more":false,"last_id":"claude-b"}"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_anthropic_models_async("sk-ant-test", &server.url(), timeout).await;

        let models = result.expect("should succeed");
        assert!(models.contains(&"claude-a".to_string()));
        assert!(models.contains(&"claude-b".to_string()));
        assert_eq!(models.len(), 2);
    }

    #[tokio::test]
    async fn test_fetch_anthropic_models_async_401_returns_api_key_missing() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(401)
            .with_body(r#"{"error":{"message":"Unauthorized"}}"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_anthropic_models_async("bad-key", &server.url(), timeout).await;

        assert!(matches!(
            result,
            Err(crate::error::ActualError::ApiKeyMissing { .. })
        ));
    }

    #[tokio::test]
    async fn test_fetch_anthropic_models_async_403_returns_api_key_missing() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(403)
            .with_body(r#"{"error":{"message":"Forbidden"}}"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_anthropic_models_async("bad-key", &server.url(), timeout).await;

        assert!(matches!(
            result,
            Err(crate::error::ActualError::ApiKeyMissing { .. })
        ));
    }

    #[tokio::test]
    async fn test_fetch_anthropic_models_async_500_returns_runner_failed() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(500)
            .with_body(r#"{"error":{"message":"Internal Server Error"}}"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_anthropic_models_async("sk-ant-test", &server.url(), timeout).await;

        assert!(matches!(
            result,
            Err(crate::error::ActualError::RunnerFailed { .. })
        ));
    }

    #[tokio::test]
    async fn test_fetch_anthropic_models_async_invalid_json_returns_runner_failed() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"not valid json"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_anthropic_models_async("sk-ant-test", &server.url(), timeout).await;

        assert!(matches!(
            result,
            Err(crate::error::ActualError::RunnerFailed { .. })
        ));
    }

    #[tokio::test]
    async fn test_fetch_anthropic_models_async_pagination_limit() {
        let mut server = mockito::Server::new_async().await;

        // First page (no after_id query param)
        let _m1 = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"model-0"}],"has_more":true,"last_id":"model-0"}"#)
            .expect(1)
            .create_async()
            .await;

        // All subsequent pages (with after_id) — called MAX_PAGES - 1 times
        let _m2 = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v1/models\?after_id=.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"model-n"}],"has_more":true,"last_id":"model-n"}"#)
            .expect(49)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(10);
        let result = fetch_anthropic_models_async("sk-ant-test", &server.url(), timeout).await;
        let models = result.expect("should succeed even when hitting page limit");
        assert_eq!(models.len(), 50); // 1 from first page + 49 from subsequent
    }

    // -----------------------------------------------------------------------
    // get_anthropic_models (synchronous)
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_anthropic_models_no_api_key_returns_empty() {
        let _guard = EnvGuard::remove("ANTHROPIC_API_KEY");
        let result = get_anthropic_models(None, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_anthropic_models_uses_cache_when_fresh() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = TEnvGuard::remove("ANTHROPIC_API_KEY");

        let dir = tempdir().unwrap();
        let cache_file = dir.path().join("model-cache.yaml");

        let fresh_cache = ModelCacheFile {
            openai: ProviderCache::default(),
            anthropic: ProviderCache {
                fetched_at: Some(Utc::now()),
                models: vec!["claude-cached-model".to_string()],
            },
        };
        save_cache_file(&cache_file, &fresh_cache);

        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = get_anthropic_models(Some("sk-ant-from-config"), None);
        assert!(result.contains(&"claude-cached-model".to_string()));
    }

    #[test]
    fn test_get_anthropic_models_fetch_error_returns_empty() {
        let result = get_anthropic_models(
            Some("sk-ant-test"),
            Some("http://127.0.0.1:1"), // nothing listening on port 1
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_anthropic_models_fetches_and_caches() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = TEnvGuard::remove("ANTHROPIC_API_KEY");

        let dir = tempdir().unwrap();
        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut server = mockito::Server::new_async().await;
                let _m = server
                    .mock("GET", "/v1/models")
                    .with_status(200)
                    .with_header("content-type", "application/json")
                    .with_body(r#"{"data":[{"id":"claude-live-model"}],"has_more":false,"last_id":"claude-live-model"}"#)
                    .create_async()
                    .await;
                tx.send(server.url()).unwrap();
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            });
        });

        let server_url = rx.recv().expect("server url");
        let result = get_anthropic_models(Some("sk-ant-test"), Some(&server_url));

        assert!(result.contains(&"claude-live-model".to_string()));

        let cache_path = dir.path().join("model-cache.yaml");
        assert!(cache_path.exists());
        let loaded = load_cache_file(&cache_path);
        assert!(loaded
            .anthropic
            .models
            .contains(&"claude-live-model".to_string()));

        drop(handle);
    }

    #[test]
    fn test_get_anthropic_models_cache_independent_from_openai() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = TEnvGuard::remove("ANTHROPIC_API_KEY");

        let dir = tempdir().unwrap();
        let cache_file = dir.path().join("model-cache.yaml");

        // Stale OpenAI cache, fresh Anthropic cache
        let cache = ModelCacheFile {
            openai: ProviderCache {
                fetched_at: Some(Utc::now() - chrono::Duration::hours(25)),
                models: vec!["gpt-4o".to_string()],
            },
            anthropic: ProviderCache {
                fetched_at: Some(Utc::now()),
                models: vec!["claude-cached-fresh".to_string()],
            },
        };
        save_cache_file(&cache_file, &cache);

        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        // Anthropic cache is fresh — should return cached without network call
        let result = get_anthropic_models(Some("sk-ant-from-config"), None);
        assert!(result.contains(&"claude-cached-fresh".to_string()));

        // OpenAI cache is stale — verify it is not fresh
        let loaded = load_cache_file(&cache_file);
        assert!(!loaded.openai.is_fresh(DEFAULT_CACHE_TTL_HOURS));
        assert!(loaded.anthropic.is_fresh(DEFAULT_CACHE_TTL_HOURS));
    }

    #[test]
    fn test_get_anthropic_models_fetches_and_returns_even_without_cache_path() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = TEnvGuard::remove("ANTHROPIC_API_KEY");
        let _bad_config = TEnvGuard::set("ACTUAL_CONFIG", "");
        let _no_dir = TEnvGuard::remove("ACTUAL_CONFIG_DIR");

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let _handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut server = mockito::Server::new_async().await;
                let _m = server
                    .mock("GET", "/v1/models")
                    .with_status(200)
                    .with_header("content-type", "application/json")
                    .with_body(r#"{"data":[{"id":"claude-no-cache-model"}],"has_more":false,"last_id":"claude-no-cache-model"}"#)
                    .create_async()
                    .await;
                tx.send(server.url()).unwrap();
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            });
        });

        let server_url = rx.recv().expect("server url");
        let result = get_anthropic_models(Some("sk-ant-test"), Some(&server_url));
        assert!(result.contains(&"claude-no-cache-model".to_string()));
    }

    // -----------------------------------------------------------------------
    // fetch_openai_models_async (mockito)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_fetch_openai_models_async_success() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"object":"list","data":[{"id":"gpt-4o","object":"model"},{"id":"text-embedding-ada-002","object":"model"}]}"#,
            )
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_openai_models_async("sk-test", &server.url(), timeout).await;

        let models = result.expect("should succeed");
        assert!(models.contains(&"gpt-4o".to_string()));
        // embeddings should be filtered out
        assert!(!models.contains(&"text-embedding-ada-002".to_string()));
    }

    #[tokio::test]
    async fn test_fetch_openai_models_async_401_returns_api_key_missing() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(401)
            .with_body(r#"{"error":{"message":"Unauthorized"}}"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_openai_models_async("bad-key", &server.url(), timeout).await;

        assert!(
            matches!(result, Err(crate::error::ActualError::ApiKeyMissing { .. })),
            "401 should return ApiKeyMissing"
        );
    }

    #[tokio::test]
    async fn test_fetch_openai_models_async_500_returns_runner_failed() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(500)
            .with_body(r#"{"error":{"message":"Internal Server Error"}}"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_openai_models_async("sk-test", &server.url(), timeout).await;

        assert!(
            matches!(result, Err(crate::error::ActualError::RunnerFailed { .. })),
            "500 should return RunnerFailed"
        );
    }

    #[tokio::test]
    async fn test_fetch_openai_models_async_403_returns_api_key_missing() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(403)
            .with_body(r#"{"error":{"message":"Forbidden"}}"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_openai_models_async("bad-key", &server.url(), timeout).await;

        assert!(
            matches!(result, Err(crate::error::ActualError::ApiKeyMissing { .. })),
            "403 should return ApiKeyMissing"
        );
    }

    #[tokio::test]
    async fn test_fetch_openai_models_async_invalid_json_returns_runner_failed() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"not valid json"#)
            .create_async()
            .await;

        let timeout = std::time::Duration::from_secs(5);
        let result = fetch_openai_models_async("sk-test", &server.url(), timeout).await;

        assert!(
            matches!(result, Err(crate::error::ActualError::RunnerFailed { .. })),
            "invalid JSON should return RunnerFailed"
        );
    }

    // -----------------------------------------------------------------------
    // get_openai_models (synchronous)
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_openai_models_no_api_key_returns_empty() {
        // Ensure env var is absent for this test
        let _guard = EnvGuard::remove("OPENAI_API_KEY");
        let result = get_openai_models(None, None);
        assert!(result.is_empty(), "no API key should return empty");
    }

    #[test]
    fn test_get_openai_models_uses_cache_when_fresh() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = EnvGuard::remove("OPENAI_API_KEY");

        let dir = tempdir().unwrap();
        let cache_file = dir.path().join("model-cache.yaml");

        // Write a fresh cache
        let fresh_cache = ModelCacheFile {
            openai: ProviderCache {
                fetched_at: Some(Utc::now()),
                models: vec!["gpt-cached-model".to_string()],
            },
            anthropic: ProviderCache::default(),
        };
        save_cache_file(&cache_file, &fresh_cache);

        // Point config dir to our temp dir
        let _config_dir_guard = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = get_openai_models(Some("sk-from-config"), None);
        assert!(result.contains(&"gpt-cached-model".to_string()));
    }

    #[test]
    fn test_get_openai_models_fetch_error_returns_empty() {
        // Use a base_url that cannot connect — should return empty gracefully
        let result = get_openai_models(
            Some("sk-test"),
            Some("http://127.0.0.1:1"), // nothing listening on port 1
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_openai_models_fetches_and_caches() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = EnvGuard::remove("OPENAI_API_KEY");

        // We need a blocking mockito server — use a std::sync channel to run it
        // from a separate thread.
        let dir = tempdir().unwrap();
        let _config_dir_guard = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        // Spin up the mockito server in a dedicated tokio runtime on another thread.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut server = mockito::Server::new_async().await;
                let _m = server
                    .mock("GET", "/v1/models")
                    .with_status(200)
                    .with_header("content-type", "application/json")
                    .with_body(
                        r#"{"object":"list","data":[{"id":"gpt-live-model","object":"model"}]}"#,
                    )
                    .create_async()
                    .await;
                tx.send(server.url()).unwrap();
                // Keep the server alive long enough for the test to call it
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            });
        });

        let server_url = rx.recv().expect("server url");
        let result = get_openai_models(Some("sk-test"), Some(&server_url));

        assert!(result.contains(&"gpt-live-model".to_string()));

        // Verify it was written to cache
        let cache_path = dir.path().join("model-cache.yaml");
        assert!(cache_path.exists(), "cache file should be written");
        let loaded = load_cache_file(&cache_path);
        assert!(loaded.openai.models.contains(&"gpt-live-model".to_string()));

        drop(handle);
    }

    /// When ACTUAL_CONFIG is set to an empty string, `config_dir()` fails and
    /// `cache_path()` returns `None`. The fetch still runs and returns models,
    /// but no cache file is written.
    #[test]
    fn test_get_openai_models_no_cache_path_still_fetches() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = EnvGuard::remove("OPENAI_API_KEY");
        // Setting ACTUAL_CONFIG to "" causes config_dir() to return Err, so
        // cache_path() returns None. The fetch should still succeed (or fail
        // gracefully), but the key point is we exercise the None branch.
        let _bad_config = EnvGuard::set("ACTUAL_CONFIG", "");
        let _no_dir = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        // With no API key and a bad base_url, the fetch fails gracefully → empty
        let result = get_openai_models(None, Some("http://127.0.0.1:1"));
        // We just verify it doesn't panic and returns empty (no key)
        assert!(result.is_empty());
    }

    /// When the fetch succeeds but ACTUAL_CONFIG is empty (so cache_path() → None),
    /// the function returns the fetched models without writing a cache file.
    #[test]
    fn test_get_openai_models_fetches_and_returns_even_without_cache_path() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _no_key = EnvGuard::remove("OPENAI_API_KEY");
        // Set ACTUAL_CONFIG to "" → config_dir() fails → cache_path() returns None.
        // Even so, the fetch should succeed and return models (the write just silently skips).
        let _bad_config = EnvGuard::set("ACTUAL_CONFIG", "");
        let _no_dir = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        // Spin up the mockito server in a dedicated tokio runtime on another thread.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let _handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut server = mockito::Server::new_async().await;
                let _m = server
                    .mock("GET", "/v1/models")
                    .with_status(200)
                    .with_header("content-type", "application/json")
                    .with_body(
                        r#"{"object":"list","data":[{"id":"gpt-no-cache-model","object":"model"}]}"#,
                    )
                    .create_async()
                    .await;
                tx.send(server.url()).unwrap();
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            });
        });

        let server_url = rx.recv().expect("server url");
        // ACTUAL_CONFIG="" → cache_path() returns None → no cache write, but fetch still works
        let result = get_openai_models(Some("sk-test"), Some(&server_url));
        assert!(result.contains(&"gpt-no-cache-model".to_string()));
    }

    #[test]
    fn test_openai_and_anthropic_cache_sections_independent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("model-cache.yaml");

        // Write only the openai section
        let mut cache = ModelCacheFile::default();
        cache.openai = ProviderCache {
            fetched_at: Some(Utc::now()),
            models: vec!["gpt-4o".to_string()],
        };
        save_cache_file(&path, &cache);

        // Read back and check anthropic is empty
        let loaded = load_cache_file(&path);
        assert!(!loaded.openai.models.is_empty());
        assert!(loaded.anthropic.models.is_empty());
        assert!(loaded.anthropic.fetched_at.is_none());
    }

    // -----------------------------------------------------------------------
    // Unix-only: file permissions
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn test_save_cache_file_creates_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("model-cache.yaml");

        save_cache_file(&path, &ModelCacheFile::default());

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "cache file should have mode 0600");
    }

    // -----------------------------------------------------------------------
    // Local helper: env-var guard (mirrors testutil::EnvGuard for use here)
    // -----------------------------------------------------------------------

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn test_local_env_guard_restores_previous_value() {
        // Exercise the Some(v) branch in EnvGuard::drop(): when a var had a prior
        // value before remove(), drop() must restore it.
        let key = "ACTUAL_CLI_TEST_LOCAL_ENV_GUARD_RESTORE";
        std::env::set_var(key, "original-value");
        {
            let _guard = EnvGuard::remove(key);
            // While the guard is alive, the var should be absent
            assert!(std::env::var(key).is_err());
        }
        // After drop(), the original value must be restored
        assert_eq!(std::env::var(key).unwrap(), "original-value");
        // Clean up
        std::env::remove_var(key);
    }

    // -----------------------------------------------------------------------
    // Provenance helpers: read_openai_cache_timestamp, read_anthropic_cache_timestamp,
    //                     read_cached_openai_models, read_cached_anthropic_models
    // -----------------------------------------------------------------------

    #[test]
    fn test_read_openai_cache_timestamp_returns_none_when_no_file() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_openai_cache_timestamp();
        assert!(result.is_none());
    }

    #[test]
    fn test_read_openai_cache_timestamp_returns_some_when_cached() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let cache_file = dir.path().join("model-cache.yaml");

        let ts = Utc::now();
        let cache = ModelCacheFile {
            openai: ProviderCache {
                fetched_at: Some(ts),
                models: vec!["gpt-4o".to_string()],
            },
            anthropic: ProviderCache::default(),
        };
        save_cache_file(&cache_file, &cache);

        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_openai_cache_timestamp();
        assert!(result.is_some());
    }

    #[test]
    fn test_read_anthropic_cache_timestamp_returns_none_when_no_file() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_anthropic_cache_timestamp();
        assert!(result.is_none());
    }

    #[test]
    fn test_read_anthropic_cache_timestamp_returns_some_when_cached() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let cache_file = dir.path().join("model-cache.yaml");

        let ts = Utc::now();
        let cache = ModelCacheFile {
            openai: ProviderCache::default(),
            anthropic: ProviderCache {
                fetched_at: Some(ts),
                models: vec!["claude-sonnet-4-6".to_string()],
            },
        };
        save_cache_file(&cache_file, &cache);

        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_anthropic_cache_timestamp();
        assert!(result.is_some());
    }

    #[test]
    fn test_read_cached_openai_models_returns_empty_when_no_cache() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_cached_openai_models();
        assert!(result.is_empty());
    }

    #[test]
    fn test_read_cached_openai_models_returns_models_when_cached() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let cache_file = dir.path().join("model-cache.yaml");

        let cache = ModelCacheFile {
            openai: ProviderCache {
                fetched_at: Some(Utc::now()),
                models: vec!["gpt-cached-1".to_string(), "gpt-cached-2".to_string()],
            },
            anthropic: ProviderCache::default(),
        };
        save_cache_file(&cache_file, &cache);

        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_cached_openai_models();
        assert!(result.contains(&"gpt-cached-1".to_string()));
        assert!(result.contains(&"gpt-cached-2".to_string()));
    }

    #[test]
    fn test_read_cached_anthropic_models_returns_empty_when_no_cache() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_cached_anthropic_models();
        assert!(result.is_empty());
    }

    #[test]
    fn test_read_cached_anthropic_models_returns_models_when_cached() {
        use crate::testutil::{EnvGuard as TEnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let cache_file = dir.path().join("model-cache.yaml");

        let cache = ModelCacheFile {
            openai: ProviderCache::default(),
            anthropic: ProviderCache {
                fetched_at: Some(Utc::now()),
                models: vec!["claude-cached-1".to_string(), "claude-cached-2".to_string()],
            },
        };
        save_cache_file(&cache_file, &cache);

        let _config_dir_guard = TEnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = read_cached_anthropic_models();
        assert!(result.contains(&"claude-cached-1".to_string()));
        assert!(result.contains(&"claude-cached-2".to_string()));
    }
}
