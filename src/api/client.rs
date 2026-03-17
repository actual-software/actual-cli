use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

use crate::analysis::signals::CanonicalIR;
use crate::analysis::types::{FrameworkCategory, Language, RepoAnalysis};
use crate::api::types::{
    ApiErrorResponse, CanonicalIRFacet, CanonicalIRPayload, CategoriesResponse, FrameworksResponse,
    HealthResponse, LanguagesResponse, MatchFramework, MatchOptions, MatchProject, MatchRequest,
    MatchResponse, TelemetryRequest,
};
use crate::config::types::Config;
use crate::error::ActualError;

pub const DEFAULT_API_URL: &str = "https://api-service.api.prod.actual.ai";

const USER_AGENT_VALUE: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub struct ActualApiClient {
    client: reqwest::Client,
    base_url: String,
}

impl ActualApiClient {
    /// Default overall request timeout (30 seconds).
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
    /// Default connection establishment timeout (10 seconds).
    const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    pub fn new(base_url: &str) -> Result<Self, ActualError> {
        Self::new_with_timeout(
            base_url,
            Self::DEFAULT_TIMEOUT,
            Self::DEFAULT_CONNECT_TIMEOUT,
        )
    }

    pub fn new_with_timeout(
        base_url: &str,
        timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self, ActualError> {
        // Enforce HTTPS for all non-loopback URLs to protect credentials in transit.
        let is_localhost = base_url.starts_with("http://localhost")
            || base_url.starts_with("http://127.0.0.1")
            || base_url.starts_with("http://[::1]");
        if !base_url.starts_with("https://") && !is_localhost {
            return Err(ActualError::ConfigError(
                "API URL must use HTTPS (got a non-HTTPS URL). \
                 Use https:// to protect your credentials."
                    .to_string(),
            ));
        }

        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .https_only(!is_localhost)
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            .build()
            .map_err(|e| ActualError::ApiError(format!("Failed to build HTTP client: {e}")))?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ActualError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response(response).await
    }

    async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ActualError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response(response).await
    }

    pub async fn post_match(&self, request: &MatchRequest) -> Result<MatchResponse, ActualError> {
        self.post("/adrs/match", request).await
    }

    pub async fn get_languages(&self) -> Result<LanguagesResponse, ActualError> {
        self.get("/taxonomy/languages").await
    }

    pub async fn get_frameworks(&self) -> Result<FrameworksResponse, ActualError> {
        self.get("/taxonomy/frameworks").await
    }

    pub async fn get_categories(&self) -> Result<CategoriesResponse, ActualError> {
        self.get("/taxonomy/categories").await
    }

    pub async fn get_health(&self) -> Result<HealthResponse, ActualError> {
        self.get("/health").await
    }

    pub async fn post_telemetry(
        &self,
        request: &TelemetryRequest,
        service_key: &str,
    ) -> Result<(), ActualError> {
        let url = format!("{}/counter/record", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {service_key}"))
            .json(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response_no_body(response).await
    }

    async fn handle_response_no_body(response: reqwest::Response) -> Result<(), ActualError> {
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(Self::map_error_response(status, response).await)
        }
    }

    async fn handle_response<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, ActualError> {
        let status = response.status();
        if status.is_success() {
            response
                .json::<T>()
                .await
                .map_err(|e| ActualError::ApiError(e.to_string()))
        } else {
            Err(Self::map_error_response(status, response).await)
        }
    }

    async fn map_error_response(
        status: reqwest::StatusCode,
        response: reqwest::Response,
    ) -> ActualError {
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return ActualError::ServiceUnavailable;
        }
        if status.is_client_error() {
            match response.json::<ApiErrorResponse>().await {
                Ok(error_response) => ActualError::ApiResponseError {
                    code: error_response.error.code,
                    message: error_response.error.message,
                },
                Err(e) => ActualError::ApiError(format!(
                    "HTTP {status}: failed to parse error response: {e}"
                )),
            }
        } else {
            let body_bytes = response.bytes().await.unwrap_or_default();
            let truncated = &body_bytes[..body_bytes.len().min(4096)];
            let body = String::from_utf8_lossy(truncated);
            ActualError::ApiError(format!("HTTP {status}: {body}"))
        }
    }
}

/// Serialize a [`Language`] enum variant to its lowercase string representation.
///
/// Uses the direct `as_str()` method which produces lowercase strings like
/// `"typescript"`, `"rust"`, etc.
fn serialize_language(lang: &Language) -> String {
    lang.as_str().to_string()
}

/// Serialize a [`FrameworkCategory`] enum variant to its kebab-case string representation.
///
/// Uses the direct `as_str()` method which produces kebab-case strings for known variants
/// (e.g. `"web-frontend"`, `"web-backend"`) and the raw string for `Other`.
fn serialize_framework_category(category: &FrameworkCategory) -> String {
    category.as_str().to_string()
}

/// Convert a [`RepoAnalysis`] and [`Config`] into a [`MatchRequest`] suitable for the API.
///
/// Maps each analyzed project to a [`MatchProject`] with serialized language and framework
/// strings, and builds [`MatchOptions`] from the config's filtering fields.
///
/// `signals` maps project path → [`CanonicalIR`] from the signals pipeline. Pass an empty
/// map when signals analysis was not run or not available.
pub fn build_match_request(
    analysis: &RepoAnalysis,
    config: &Config,
    signals: &HashMap<String, CanonicalIR>,
) -> MatchRequest {
    let projects = analysis
        .projects
        .iter()
        .map(|project| {
            let (languages, frameworks) = if let Some(ref sel) = project.selection {
                // Selection exists: send only the selected language + framework
                let langs = vec![serialize_language(&sel.language.language)];
                let fws = sel
                    .framework
                    .iter()
                    .map(|fw| MatchFramework {
                        name: fw.name.clone(),
                        category: serialize_framework_category(&fw.category),
                    })
                    .collect();
                (langs, fws)
            } else {
                // No selection (backward compat): send all
                let langs = project
                    .languages
                    .iter()
                    .map(|ls| serialize_language(&ls.language))
                    .collect();
                let fws = project
                    .frameworks
                    .iter()
                    .map(|fw| MatchFramework {
                        name: fw.name.clone(),
                        category: serialize_framework_category(&fw.category),
                    })
                    .collect();
                (langs, fws)
            };

            let canonical_ir = signals.get(&project.path).map(|ir| CanonicalIRPayload {
                ir_hash: ir.ir_hash.clone(),
                taxonomy_version: ir.taxonomy_version.clone(),
                facets: ir
                    .facets_by_leaf_id
                    .values()
                    .map(|f| CanonicalIRFacet {
                        facet_slot: f.facet_slot.clone(),
                        rule_ids: f.rule_ids.clone(),
                        confidence: f.confidence,
                    })
                    .collect(),
            });

            MatchProject {
                path: project.path.clone(),
                name: project.name.clone(),
                languages,
                frameworks,
                canonical_ir,
            }
        })
        .collect();

    let options = if config.include_categories.is_some()
        || config.exclude_categories.is_some()
        || config.include_general.is_some()
        || config.max_per_framework.is_some()
    {
        Some(MatchOptions {
            categories: config.include_categories.clone(),
            exclude_categories: config.exclude_categories.clone(),
            include_general: config.include_general,
            max_per_framework: config.max_per_framework,
        })
    } else {
        None
    };

    MatchRequest { projects, options }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{
        Framework, FrameworkCategory, Language, LanguageStat, Project, ProjectSelection,
        RepoAnalysis, WorkspaceType,
    };

    /// Helper: create a minimal project with the given languages and frameworks.
    fn make_project(
        path: &str,
        name: &str,
        languages: Vec<Language>,
        frameworks: Vec<Framework>,
    ) -> Project {
        Project {
            path: path.to_string(),
            name: name.to_string(),
            languages: languages
                .into_iter()
                .map(|language| LanguageStat { language, loc: 0 })
                .collect(),
            frameworks,
            package_manager: None,
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
            selection: None,
        }
    }

    #[test]
    fn test_monorepo_three_projects() {
        let analysis = RepoAnalysis {
            is_monorepo: true,
            workspace_type: Some(WorkspaceType::Pnpm),
            projects: vec![
                make_project("apps/web", "web", vec![Language::TypeScript], vec![]),
                make_project("apps/api", "api", vec![Language::Rust], vec![]),
                make_project("libs/shared", "shared", vec![Language::JavaScript], vec![]),
            ],
        };

        let request = build_match_request(&analysis, &Config::default(), &HashMap::new());
        assert_eq!(request.projects.len(), 3);
    }

    #[test]
    fn test_config_with_include_categories() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };

        let config = Config {
            include_categories: Some(vec!["security".to_string(), "testing".to_string()]),
            ..Config::default()
        };

        let request = build_match_request(&analysis, &config, &HashMap::new());
        assert!(request.options.is_some());
        let options = request.options.unwrap();
        assert_eq!(
            options.categories,
            Some(vec!["security".to_string(), "testing".to_string()])
        );
    }

    #[test]
    fn test_config_with_max_per_framework() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };
        let config = Config {
            max_per_framework: Some(5),
            ..Config::default()
        };
        let request = build_match_request(&analysis, &config, &HashMap::new());
        assert!(request.options.is_some());
        let options = request.options.unwrap();
        assert_eq!(options.max_per_framework, Some(5));
    }

    #[test]
    fn test_default_config_options_none() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };

        let request = build_match_request(&analysis, &Config::default(), &HashMap::new());
        assert!(request.options.is_none());
    }

    #[test]
    fn test_language_typescript_serializes_correctly() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(
                ".",
                "app",
                vec![Language::TypeScript],
                vec![Framework {
                    name: "nextjs".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: None,
                }],
            )],
        };

        let request = build_match_request(&analysis, &Config::default(), &HashMap::new());
        assert!(request.projects[0]
            .languages
            .contains(&"typescript".to_string()));
    }

    // --- ActualApiClient tests ---

    fn sample_match_request() -> MatchRequest {
        MatchRequest {
            projects: vec![MatchProject {
                path: "apps/web".to_string(),
                name: "web-app".to_string(),
                languages: vec!["typescript".to_string()],
                frameworks: vec![MatchFramework {
                    name: "nextjs".to_string(),
                    category: "frontend".to_string(),
                }],
                canonical_ir: None,
            }],
            options: None,
        }
    }

    #[tokio::test]
    async fn test_post_match_success() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "matched_adrs": [{
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "title": "Use App Router for all new pages",
                "context": "Next.js 13+ introduced the App Router",
                "policies": ["Use the App Router (app/ directory) for all new pages"],
                "instructions": ["New routes go in app/"],
                "category": {"id": "cat-123", "name": "Rendering Model", "path": "Frontend > Rendering Model"},
                "applies_to": {"languages": ["typescript"], "frameworks": ["nextjs"]},
                "matched_projects": ["apps/web"]
            }],
            "metadata": {"total_matched": 1, "by_framework": {"nextjs": 1}, "deduplicated_count": 1}
        }"#;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.matched_adrs.len(), 1);
        assert_eq!(
            response.matched_adrs[0].id,
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(
            response.matched_adrs[0].title,
            "Use App Router for all new pages"
        );
        assert_eq!(response.metadata.total_matched, 1);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_api_error_response() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{"error": {"code": "INVALID_REQUEST", "message": "At least one project is required", "details": null}}"#;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiResponseError { ref code, ref message } if code == "INVALID_REQUEST" && message == "At least one project is required")
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_server_error() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("500"))
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_service_unavailable() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(503)
            .with_body("Service Unavailable")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ServiceUnavailable),
            "expected ServiceUnavailable for 503 response"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_user_agent_header_sent() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "matched_adrs": [],
            "metadata": {"total_matched": 0, "by_framework": {}, "deduplicated_count": 0}
        }"#;

        let mock = server
            .mock("POST", "/adrs/match")
            .match_header("user-agent", mockito::Matcher::Regex("actual-cli/".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_client_error_unparseable_body() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(422)
            .with_body("not valid json")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("422") && msg.contains("failed to parse error response"))
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_success_unparseable_response() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not valid json")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("error decoding response body"))
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_network_error() {
        let client = ActualApiClient::new("http://127.0.0.1:1").unwrap();
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ApiError(_)));
    }

    #[test]
    fn test_new_trims_trailing_slash() {
        let client = ActualApiClient::new("https://example.com/").unwrap();
        assert_eq!(client.base_url, "https://example.com");
    }

    #[test]
    fn test_new_returns_valid_client() {
        let client = ActualApiClient::new("https://example.com");
        assert!(client.is_ok());
    }

    #[test]
    fn test_new_rejects_http_non_localhost() {
        let result = ActualApiClient::new("http://example.com");
        assert!(
            matches!(result, Err(ActualError::ConfigError(ref msg)) if msg.contains("HTTPS")),
            "expected ConfigError for HTTP non-localhost URL, got: {:?}",
            result
        );
    }

    #[test]
    fn test_new_allows_localhost_http() {
        let result = ActualApiClient::new("http://127.0.0.1:8080");
        assert!(
            result.is_ok(),
            "localhost HTTP should be allowed for testing"
        );
    }

    #[test]
    fn test_default_api_url() {
        assert_eq!(DEFAULT_API_URL, "https://api-service.api.prod.actual.ai");
    }

    #[test]
    fn test_user_agent_value() {
        assert!(USER_AGENT_VALUE.starts_with("actual-cli/"));
    }

    // --- get_languages tests ---

    #[tokio::test]
    async fn test_get_languages_success() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "languages": [
                {"id": "rust", "display_name": "Rust", "aliases": ["rs"]},
                {"id": "typescript", "display_name": "TypeScript", "aliases": ["ts", "tsx"]}
            ]
        }"#;

        let mock = server
            .mock("GET", "/taxonomy/languages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_languages().await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.languages.len(), 2);
        assert_eq!(response.languages[0].id, "rust");
        assert_eq!(response.languages[0].display_name, "Rust");
        assert_eq!(response.languages[0].aliases, vec!["rs"]);
        assert_eq!(response.languages[1].id, "typescript");
        assert_eq!(response.languages[1].display_name, "TypeScript");
        assert_eq!(response.languages[1].aliases, vec!["ts", "tsx"]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_languages_api_error() {
        let mut server = mockito::Server::new_async().await;
        let body =
            r#"{"error": {"code": "UNAUTHORIZED", "message": "Invalid API key", "details": null}}"#;

        let mock = server
            .mock("GET", "/taxonomy/languages")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_languages().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiResponseError { ref code, ref message } if code == "UNAUTHORIZED" && message == "Invalid API key")
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_languages_server_error() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", "/taxonomy/languages")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_languages().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("500"))
        );
        mock.assert_async().await;
    }

    // --- get_frameworks tests ---

    #[tokio::test]
    async fn test_get_frameworks_success() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "frameworks": [
                {
                    "id": "nextjs",
                    "display_name": "Next.js",
                    "category": "frontend",
                    "languages": ["typescript", "javascript"]
                },
                {
                    "id": "actix-web",
                    "display_name": "Actix Web",
                    "category": "backend",
                    "languages": ["rust"]
                }
            ]
        }"#;

        let mock = server
            .mock("GET", "/taxonomy/frameworks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_frameworks().await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.frameworks.len(), 2);
        assert_eq!(response.frameworks[0].id, "nextjs");
        assert_eq!(response.frameworks[0].display_name, "Next.js");
        assert_eq!(response.frameworks[0].category, "frontend");
        assert_eq!(
            response.frameworks[0].languages,
            vec!["typescript", "javascript"]
        );
        assert_eq!(response.frameworks[1].id, "actix-web");
        assert_eq!(response.frameworks[1].display_name, "Actix Web");
        assert_eq!(response.frameworks[1].category, "backend");
        assert_eq!(response.frameworks[1].languages, vec!["rust"]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_frameworks_api_error() {
        let mut server = mockito::Server::new_async().await;
        let body =
            r#"{"error": {"code": "UNAUTHORIZED", "message": "Invalid API key", "details": null}}"#;

        let mock = server
            .mock("GET", "/taxonomy/frameworks")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_frameworks().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiResponseError { ref code, ref message } if code == "UNAUTHORIZED" && message == "Invalid API key")
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_frameworks_server_error() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", "/taxonomy/frameworks")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_frameworks().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("500"))
        );
        mock.assert_async().await;
    }

    // --- get_categories tests ---

    #[tokio::test]
    async fn test_get_categories_success() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "categories": [
                {
                    "id": "frontend",
                    "name": "Frontend",
                    "path": "Frontend",
                    "level": 0,
                    "children": [
                        {
                            "id": "rendering",
                            "name": "Rendering Model",
                            "path": "Frontend > Rendering Model",
                            "level": 1,
                            "children": [
                                {
                                    "id": "ssr",
                                    "name": "Server-Side Rendering",
                                    "path": "Frontend > Rendering Model > Server-Side Rendering",
                                    "level": 2,
                                    "children": []
                                }
                            ]
                        },
                        {
                            "id": "styling",
                            "name": "Styling",
                            "path": "Frontend > Styling",
                            "level": 1,
                            "children": []
                        }
                    ]
                }
            ]
        }"#;

        let mock = server
            .mock("GET", "/taxonomy/categories")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_categories().await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.categories.len(), 1);

        let frontend = &response.categories[0];
        assert_eq!(frontend.id, "frontend");
        assert_eq!(frontend.name, "Frontend");
        assert_eq!(frontend.path, "Frontend");
        assert_eq!(frontend.level, 0);
        assert_eq!(frontend.children.len(), 2);

        let rendering = &frontend.children[0];
        assert_eq!(rendering.id, "rendering");
        assert_eq!(rendering.name, "Rendering Model");
        assert_eq!(rendering.path, "Frontend > Rendering Model");
        assert_eq!(rendering.level, 1);
        assert_eq!(rendering.children.len(), 1);

        let ssr = &rendering.children[0];
        assert_eq!(ssr.id, "ssr");
        assert_eq!(ssr.name, "Server-Side Rendering");
        assert_eq!(
            ssr.path,
            "Frontend > Rendering Model > Server-Side Rendering"
        );
        assert_eq!(ssr.level, 2);
        assert!(ssr.children.is_empty());

        let styling = &frontend.children[1];
        assert_eq!(styling.id, "styling");
        assert_eq!(styling.name, "Styling");
        assert!(styling.children.is_empty());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_categories_api_error() {
        let mut server = mockito::Server::new_async().await;
        let body =
            r#"{"error": {"code": "UNAUTHORIZED", "message": "Invalid API key", "details": null}}"#;

        let mock = server
            .mock("GET", "/taxonomy/categories")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_categories().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiResponseError { ref code, ref message } if code == "UNAUTHORIZED" && message == "Invalid API key")
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_categories_server_error() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", "/taxonomy/categories")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_categories().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("500"))
        );
        mock.assert_async().await;
    }

    // --- get_health tests ---

    #[tokio::test]
    async fn test_get_health_success() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{"status": "ok", "version": "1.2.3"}"#;

        let mock = server
            .mock("GET", "/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_health().await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status, "ok");
        assert_eq!(response.version, "1.2.3");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_health_api_error() {
        let mut server = mockito::Server::new_async().await;
        let body =
            r#"{"error": {"code": "UNAUTHORIZED", "message": "Invalid API key", "details": null}}"#;

        let mock = server
            .mock("GET", "/health")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_health().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiResponseError { ref code, ref message } if code == "UNAUTHORIZED" && message == "Invalid API key")
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_health_server_error() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", "/health")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let result = client.get_health().await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("500"))
        );
        mock.assert_async().await;
    }

    // --- Network error tests for GET endpoints ---

    #[tokio::test]
    async fn test_get_languages_network_error() {
        let client = ActualApiClient::new("http://127.0.0.1:1").unwrap();
        let result = client.get_languages().await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ApiError(_)));
    }

    #[tokio::test]
    async fn test_get_frameworks_network_error() {
        let client = ActualApiClient::new("http://127.0.0.1:1").unwrap();
        let result = client.get_frameworks().await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ApiError(_)));
    }

    #[tokio::test]
    async fn test_get_categories_network_error() {
        let client = ActualApiClient::new("http://127.0.0.1:1").unwrap();
        let result = client.get_categories().await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ApiError(_)));
    }

    #[tokio::test]
    async fn test_get_health_network_error() {
        let client = ActualApiClient::new("http://127.0.0.1:1").unwrap();
        let result = client.get_health().await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ApiError(_)));
    }

    // --- post_telemetry tests ---

    #[tokio::test]
    async fn test_post_telemetry_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .match_header("authorization", "Bearer test-key")
            .match_header("content-type", "application/json")
            .with_status(200)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let request = TelemetryRequest { metrics: vec![] };
        let result = client.post_telemetry(&request, "test-key").await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_telemetry_server_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let request = TelemetryRequest { metrics: vec![] };
        let result = client.post_telemetry(&request, "test-key").await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("500"))
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_telemetry_network_error() {
        let client = ActualApiClient::new("http://127.0.0.1:1").unwrap();
        let request = TelemetryRequest { metrics: vec![] };
        let result = client.post_telemetry(&request, "test-key").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ApiError(_)));
    }

    #[tokio::test]
    async fn test_post_telemetry_api_error_response() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{"error": {"code": "UNAUTHORIZED", "message": "Invalid service key", "details": null}}"#;

        let mock = server
            .mock("POST", "/counter/record")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let request = TelemetryRequest { metrics: vec![] };
        let result = client.post_telemetry(&request, "bad-key").await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiResponseError { ref code, ref message } if code == "UNAUTHORIZED" && message == "Invalid service key")
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_telemetry_client_error_unparseable_body() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/counter/record")
            .with_status(400)
            .with_body("not valid json")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url()).unwrap();
        let request = TelemetryRequest { metrics: vec![] };
        let result = client.post_telemetry(&request, "test-key").await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("400") && msg.contains("failed to parse error response"))
        );
        mock.assert_async().await;
    }

    // --- build_match_request with ProjectSelection tests ---

    #[test]
    fn test_build_match_request_uses_selection() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "test".to_string(),
                languages: vec![
                    LanguageStat {
                        language: Language::Rust,
                        loc: 5000,
                    },
                    LanguageStat {
                        language: Language::Python,
                        loc: 200,
                    },
                ],
                frameworks: vec![
                    Framework {
                        name: "actix-web".to_string(),
                        category: FrameworkCategory::WebBackend,
                        source: None,
                    },
                    Framework {
                        name: "diesel".to_string(),
                        category: FrameworkCategory::Data,
                        source: None,
                    },
                ],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
                selection: Some(ProjectSelection {
                    language: LanguageStat {
                        language: Language::Rust,
                        loc: 5000,
                    },
                    framework: Some(Framework {
                        name: "actix-web".to_string(),
                        category: FrameworkCategory::WebBackend,
                        source: None,
                    }),
                    auto_selected: true,
                }),
            }],
        };
        let request = build_match_request(&analysis, &Config::default(), &HashMap::new());
        assert_eq!(request.projects.len(), 1);
        assert_eq!(request.projects[0].languages, vec!["rust".to_string()]);
        assert_eq!(request.projects[0].frameworks.len(), 1);
        assert_eq!(request.projects[0].frameworks[0].name, "actix-web");
    }

    #[test]
    fn test_build_match_request_no_selection_sends_all() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "test".to_string(),
                languages: vec![
                    LanguageStat {
                        language: Language::Rust,
                        loc: 5000,
                    },
                    LanguageStat {
                        language: Language::Python,
                        loc: 200,
                    },
                ],
                frameworks: vec![
                    Framework {
                        name: "actix-web".to_string(),
                        category: FrameworkCategory::WebBackend,
                        source: None,
                    },
                    Framework {
                        name: "diesel".to_string(),
                        category: FrameworkCategory::Data,
                        source: None,
                    },
                ],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
                selection: None,
            }],
        };
        let request = build_match_request(&analysis, &Config::default(), &HashMap::new());
        assert_eq!(request.projects.len(), 1);
        assert_eq!(request.projects[0].languages.len(), 2);
        assert!(request.projects[0].languages.contains(&"rust".to_string()));
        assert!(request.projects[0]
            .languages
            .contains(&"python".to_string()));
        assert_eq!(request.projects[0].frameworks.len(), 2);
    }

    // --- Timeout tests ---

    #[test]
    fn test_new_with_timeout_creates_client() {
        let client = ActualApiClient::new_with_timeout(
            "https://example.com",
            Duration::from_secs(5),
            Duration::from_secs(2),
        );
        assert!(client.is_ok());
    }

    #[test]
    fn test_new_with_timeout_rejects_http_non_localhost() {
        let result = ActualApiClient::new_with_timeout(
            "http://example.com",
            Duration::from_secs(5),
            Duration::from_secs(2),
        );
        assert!(matches!(result, Err(ActualError::ConfigError(ref msg)) if msg.contains("HTTPS")),);
    }

    #[test]
    fn test_new_with_timeout_allows_localhost_http() {
        let result = ActualApiClient::new_with_timeout(
            "http://127.0.0.1:8080",
            Duration::from_secs(5),
            Duration::from_secs(2),
        );
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_request_timeout() {
        // Create a TCP listener that accepts connections but never responds,
        // which will trigger the request timeout.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Keep listener alive but don't handle connections
        let _listener = listener;

        let client = ActualApiClient::new_with_timeout(
            &format!("http://127.0.0.1:{}", addr.port()),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();

        let start = std::time::Instant::now();
        let result = client.get_health().await;
        let elapsed = start.elapsed();

        assert!(result.is_err());
        // Should timeout well before 5 seconds (configured for 1 second)
        assert!(
            elapsed < Duration::from_secs(5),
            "expected timeout within 5s, took {elapsed:?}"
        );
    }

    #[test]
    fn test_default_timeout_constants() {
        assert_eq!(ActualApiClient::DEFAULT_TIMEOUT, Duration::from_secs(30));
        assert_eq!(
            ActualApiClient::DEFAULT_CONNECT_TIMEOUT,
            Duration::from_secs(10)
        );
    }

    #[test]
    fn test_serialize_language_all_variants() {
        let all_languages = vec![
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Rust,
            Language::Go,
            Language::Java,
            Language::Kotlin,
            Language::Swift,
            Language::Ruby,
            Language::Php,
            Language::C,
            Language::Cpp,
            Language::CSharp,
            Language::Scala,
            Language::Elixir,
            Language::Other("haskell".to_string()),
        ];
        for lang in &all_languages {
            let result = serialize_language(lang);
            assert!(
                !result.is_empty(),
                "serialize_language should return non-empty string for {lang:?}"
            );
        }
    }

    #[test]
    fn test_serialize_framework_category_all_variants() {
        let all_categories = vec![
            FrameworkCategory::WebFrontend,
            FrameworkCategory::WebBackend,
            FrameworkCategory::Mobile,
            FrameworkCategory::Desktop,
            FrameworkCategory::Cli,
            FrameworkCategory::Library,
            FrameworkCategory::Data,
            FrameworkCategory::Ml,
            FrameworkCategory::Devops,
            FrameworkCategory::Testing,
            FrameworkCategory::BuildSystem,
            FrameworkCategory::Other("embedded".to_string()),
        ];
        for cat in &all_categories {
            let result = serialize_framework_category(cat);
            assert!(
                !result.is_empty(),
                "serialize_framework_category should return non-empty string for {cat:?}"
            );
        }
    }

    #[test]
    fn test_build_match_request_selection_no_framework() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "test".to_string(),
                languages: vec![
                    LanguageStat {
                        language: Language::Rust,
                        loc: 5000,
                    },
                    LanguageStat {
                        language: Language::Python,
                        loc: 200,
                    },
                ],
                frameworks: vec![Framework {
                    name: "actix-web".to_string(),
                    category: FrameworkCategory::WebBackend,
                    source: None,
                }],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
                selection: Some(ProjectSelection {
                    language: LanguageStat {
                        language: Language::Python,
                        loc: 200,
                    },
                    framework: None,
                    auto_selected: false,
                }),
            }],
        };
        let request = build_match_request(&analysis, &Config::default(), &HashMap::new());
        assert_eq!(request.projects.len(), 1);
        assert_eq!(request.projects[0].languages, vec!["python".to_string()]);
        assert!(request.projects[0].frameworks.is_empty());
    }
}
