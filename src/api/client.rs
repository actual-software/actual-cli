use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

use crate::analysis::types::{FrameworkCategory, Language, RepoAnalysis};
use crate::api::types::{
    ApiErrorResponse, CategoriesResponse, FrameworksResponse, HealthResponse, LanguagesResponse,
    MatchFramework, MatchOptions, MatchProject, MatchRequest, MatchResponse, TelemetryRequest,
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
    pub fn new(base_url: &str) -> Result<Self, ActualError> {
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
            .build()
            .map_err(|e| ActualError::ApiError(format!("Failed to build HTTP client: {e}")))?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub async fn post_match(&self, request: &MatchRequest) -> Result<MatchResponse, ActualError> {
        let url = format!("{}/adrs/match", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response(response).await
    }

    pub async fn get_languages(&self) -> Result<LanguagesResponse, ActualError> {
        let url = format!("{}/taxonomy/languages", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response(response).await
    }

    pub async fn get_frameworks(&self) -> Result<FrameworksResponse, ActualError> {
        let url = format!("{}/taxonomy/frameworks", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response(response).await
    }

    pub async fn get_categories(&self) -> Result<CategoriesResponse, ActualError> {
        let url = format!("{}/taxonomy/categories", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response(response).await
    }

    pub async fn get_health(&self) -> Result<HealthResponse, ActualError> {
        let url = format!("{}/health", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        Self::handle_response(response).await
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

/// Serialize a [`Language`] enum variant to its serde string representation.
///
/// Uses `serde_json::to_value` which produces lowercase strings like
/// `"typescript"`, `"rust"`, etc.
fn serialize_language(lang: &Language) -> String {
    serde_json::to_value(lang)
        .expect("Language serialization cannot fail")
        .as_str()
        .expect("Language serializes as string")
        .to_string()
}

/// Serialize a [`FrameworkCategory`] enum variant to its serde string representation.
///
/// Uses `serde_json::to_value` which produces kebab-case strings for known variants
/// (e.g. `"web-frontend"`, `"web-backend"`) and the raw string for `Other`.
fn serialize_framework_category(category: &FrameworkCategory) -> String {
    serde_json::to_value(category)
        .expect("FrameworkCategory serialization cannot fail")
        .as_str()
        .expect("FrameworkCategory serializes as string")
        .to_string()
}

/// Convert a [`RepoAnalysis`] and [`Config`] into a [`MatchRequest`] suitable for the API.
///
/// Maps each analyzed project to a [`MatchProject`] with serialized language and framework
/// strings, and builds [`MatchOptions`] from the config's filtering fields.
pub fn build_match_request(analysis: &RepoAnalysis, config: &Config) -> MatchRequest {
    let projects = analysis
        .projects
        .iter()
        .map(|project| MatchProject {
            path: project.path.clone(),
            name: project.name.clone(),
            languages: project
                .languages
                .iter()
                .map(|ls| serialize_language(&ls.language))
                .collect(),
            frameworks: project
                .frameworks
                .iter()
                .map(|fw| MatchFramework {
                    name: fw.name.clone(),
                    category: serialize_framework_category(&fw.category),
                })
                .collect(),
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
        Framework, FrameworkCategory, Language, LanguageStat, Project, RepoAnalysis,
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
        }
    }

    #[test]
    fn test_monorepo_three_projects() {
        let analysis = RepoAnalysis {
            is_monorepo: true,
            projects: vec![
                make_project("apps/web", "web", vec![Language::TypeScript], vec![]),
                make_project("apps/api", "api", vec![Language::Rust], vec![]),
                make_project("libs/shared", "shared", vec![Language::JavaScript], vec![]),
            ],
        };

        let request = build_match_request(&analysis, &Config::default());
        assert_eq!(request.projects.len(), 3);
    }

    #[test]
    fn test_config_with_include_categories() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };

        let config = Config {
            include_categories: Some(vec!["security".to_string(), "testing".to_string()]),
            ..Config::default()
        };

        let request = build_match_request(&analysis, &config);
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
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };
        let config = Config {
            max_per_framework: Some(5),
            ..Config::default()
        };
        let request = build_match_request(&analysis, &config);
        assert!(request.options.is_some());
        let options = request.options.unwrap();
        assert_eq!(options.max_per_framework, Some(5));
    }

    #[test]
    fn test_default_config_options_none() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };

        let request = build_match_request(&analysis, &Config::default());
        assert!(request.options.is_none());
    }

    #[test]
    fn test_language_typescript_serializes_correctly() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![make_project(
                ".",
                "app",
                vec![Language::TypeScript],
                vec![Framework {
                    name: "nextjs".to_string(),
                    category: FrameworkCategory::WebFrontend,
                }],
            )],
        };

        let request = build_match_request(&analysis, &Config::default());
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
}
