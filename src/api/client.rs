use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

use crate::analysis::types::{FrameworkCategory, Language, RepoAnalysis};
use crate::api::types::{
    ApiErrorResponse, MatchFramework, MatchOptions, MatchProject, MatchRequest, MatchResponse,
};
use crate::config::types::Config;
use crate::error::ActualError;

pub const DEFAULT_API_URL: &str = "https://api-service.api.prod.actual.ai";

const USER_AGENT_VALUE: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

pub struct ActualApiClient {
    client: reqwest::Client,
    base_url: String,
}

impl ActualApiClient {
    pub fn new(base_url: &str) -> Self {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
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

        let status = response.status();

        if status.is_success() {
            response
                .json::<MatchResponse>()
                .await
                .map_err(|e| ActualError::ApiError(e.to_string()))
        } else if status.is_client_error() {
            match response.json::<ApiErrorResponse>().await {
                Ok(error_response) => Err(ActualError::ApiResponseError {
                    code: error_response.error.code,
                    message: error_response.error.message,
                }),
                Err(e) => Err(ActualError::ApiError(format!(
                    "HTTP {status}: failed to parse error response: {e}"
                ))),
            }
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(ActualError::ApiError(format!("HTTP {status}: {body}")))
        }
    }
}

/// Serialize a [`Language`] enum variant to its serde string representation.
///
/// Uses `serde_json::to_value` which, combined with `#[serde(rename_all = "lowercase")]`,
/// produces lowercase strings like `"typescript"`, `"rust"`, etc.
fn serialize_language(lang: &Language) -> String {
    serde_json::to_value(lang)
        .expect("Language serialization cannot fail")
        .as_str()
        .expect("Language serializes as string")
        .to_string()
}

/// Serialize a [`FrameworkCategory`] enum variant to its serde string representation.
///
/// Uses `serde_json::to_value` which, combined with `#[serde(rename_all = "kebab-case")]`,
/// produces kebab-case strings like `"web-frontend"`, `"web-backend"`, etc.
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
            languages: project.languages.iter().map(serialize_language).collect(),
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
    {
        Some(MatchOptions {
            categories: config.include_categories.clone(),
            exclude_categories: config.exclude_categories.clone(),
            include_general: config.include_general,
            max_per_framework: None,
        })
    } else {
        None
    };

    MatchRequest { projects, options }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, Project, RepoAnalysis};

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
            languages,
            frameworks,
            package_manager: None,
            description: None,
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

        let client = ActualApiClient::new(&server.url());
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

        let client = ActualApiClient::new(&server.url());
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

        let client = ActualApiClient::new(&server.url());
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

        let client = ActualApiClient::new(&server.url());
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

        let client = ActualApiClient::new(&server.url());
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

        let client = ActualApiClient::new(&server.url());
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ActualError::ApiError(ref msg) if msg.contains("error decoding response body"))
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_network_error() {
        let client = ActualApiClient::new("http://127.0.0.1:1");
        let result = client.post_match(&sample_match_request()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ApiError(_)));
    }

    #[test]
    fn test_new_trims_trailing_slash() {
        let client = ActualApiClient::new("http://example.com/");
        assert_eq!(client.base_url, "http://example.com");
    }

    #[test]
    fn test_new_returns_valid_client() {
        let _client = ActualApiClient::new("http://localhost");
    }

    #[test]
    fn test_default_api_url() {
        assert_eq!(DEFAULT_API_URL, "https://api-service.api.prod.actual.ai");
    }

    #[test]
    fn test_user_agent_value() {
        assert!(USER_AGENT_VALUE.starts_with("actual-cli/"));
    }
}
