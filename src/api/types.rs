use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Match request types (POST /adrs/match) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchRequest {
    pub projects: Vec<MatchProject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<MatchOptions>,
}

/// Compact representation of a single facet from a project's CanonicalIR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalIRFacet {
    pub facet_slot: String,
    pub rule_ids: Vec<String>,
    pub confidence: f64,
}

/// Compact representation of a project's CanonicalIR for the match request.
/// Contains only what the server needs for matching: facet slots and rule IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalIRPayload {
    pub ir_hash: String,
    pub taxonomy_version: String,
    pub facets: Vec<CanonicalIRFacet>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchProject {
    pub path: String,
    pub name: String,
    pub languages: Vec<String>,
    pub frameworks: Vec<MatchFramework>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_ir: Option<CanonicalIRPayload>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchFramework {
    pub name: String,
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchOptions {
    pub include_general: Option<bool>,
    pub categories: Option<Vec<String>>,
    pub exclude_categories: Option<Vec<String>>,
    pub max_per_framework: Option<u32>,
}

// --- Match response types ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchResponse {
    pub matched_adrs: Vec<Adr>,
    pub metadata: MatchMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Adr {
    pub id: String,
    pub title: String,
    pub context: Option<String>,
    pub policies: Vec<String>,
    pub instructions: Option<Vec<String>>,
    pub category: AdrCategory,
    pub applies_to: AppliesTo,
    pub matched_projects: Vec<String>,
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub content_md: Option<String>,
    #[serde(default)]
    pub content_json: Option<serde_json::Value>,
    #[serde(default)]
    pub source: Option<String>,
}

impl Adr {
    pub fn is_v2(&self) -> bool {
        self.schema_version.is_some_and(|v| v >= 2) || self.content_md.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AdrCategory {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppliesTo {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub version_range: Option<String>,
    #[serde(default)]
    pub facets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchMetadata {
    pub total_matched: u32,
    pub by_framework: HashMap<String, u32>,
    pub deduplicated_count: u32,
}

// --- Error types ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: ApiErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

// --- Taxonomy types (GET /taxonomy/*) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanguagesResponse {
    pub languages: Vec<LanguageInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanguageInfo {
    pub id: String,
    pub display_name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameworksResponse {
    pub frameworks: Vec<FrameworkInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameworkInfo {
    pub id: String,
    pub display_name: String,
    pub category: String,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoriesResponse {
    pub categories: Vec<CategoryNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub level: u32,
    pub children: Vec<CategoryNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

// --- Connected-repos types (GET /v1/connected-repos) ---

/// One repository connected to an organization. Fields are snake_case to match
/// the other CLI-facing (advisor) response bodies. Lets a client map a
/// repository `name` (or `external_owner`/`name`) to its `repo_unique_id`.
/// Extra server fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConnectedRepository {
    pub repo_unique_id: String,
    pub name: String,
    pub external_owner: String,
    pub url: String,
}

/// Response body for `GET /v1/connected-repos`. `repositories` defaults to empty
/// when the org has none connected.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetConnectedReposResponse {
    #[serde(default)]
    pub repositories: Vec<ConnectedRepository>,
}

/// Flat error body returned by `GET /v1/connected-repos` (`{error, message}`),
/// distinct from the advisor routes' nested [`ApiErrorResponse`]. Only the
/// human-readable `message` is consumed (the sibling `error` code is ignored);
/// it defaults to empty so a partial or unexpected body still deserializes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConnectedReposErrorBody {
    #[serde(default)]
    pub message: String,
}

// --- Telemetry types (POST /counter/record) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryRequest {
    pub metrics: Vec<TelemetryMetric>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryMetric {
    pub name: String,
    pub value: u64,
    pub tags: HashMap<String, String>,
}

// --- Advisor query types (POST /v1/advisor/query, GET /v1/advisor/query/:id) ---

/// The client surface initiating an advisor query. The CLI always sends `cli`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvisorSurface {
    pub kind: String,
}

impl AdvisorSurface {
    /// The CLI surface (`{ "kind": "cli" }`).
    pub fn cli() -> Self {
        Self {
            kind: "cli".to_string(),
        }
    }
}

/// Result-delivery sink. The CLI polls for its own result, so it always sends
/// `none`; the other sink types exist server-side for the Slack/GitHub surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdvisorSink {
    None,
}

/// Job-envelope discriminator. The server validates `type` as the exact literal
/// `advisor_query`, so this serializes to that string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorJobType {
    AdvisorQuery,
}

/// Versioned `data` payload for an advisor-query job (server contract v1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdvisorQueryData {
    pub query: String,
}

/// Fixed job-data version the server validates as the literal `1`.
const ADVISOR_QUERY_VERSION_V1: u8 = 1;

/// Request body for `POST /v1/advisor/query`.
///
/// The server expects a typed, versioned job envelope: `type`/`version` literals
/// plus the query nested under `data`. Build with [`AdvisorQueryRequest::new`]
/// so those invariants live in one place.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdvisorQueryRequest {
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_unique_id: Option<String>,
    #[serde(rename = "type")]
    pub job_type: AdvisorJobType,
    pub version: u8,
    pub data: AdvisorQueryData,
    pub surface: AdvisorSurface,
    pub sink: AdvisorSink,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl AdvisorQueryRequest {
    /// Build a v1 advisor-query request. `type` and `version` are fixed by the
    /// server's job-envelope contract; `query` is nested under `data`.
    pub fn new(
        org_id: String,
        repo_unique_id: Option<String>,
        query: String,
        surface: AdvisorSurface,
        sink: AdvisorSink,
        idempotency_key: Option<String>,
    ) -> Self {
        Self {
            org_id,
            repo_unique_id,
            job_type: AdvisorJobType::AdvisorQuery,
            version: ADVISOR_QUERY_VERSION_V1,
            data: AdvisorQueryData { query },
            surface,
            sink,
            idempotency_key,
        }
    }
}

/// Lifecycle state of an advisor job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorJobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl AdvisorJobStatus {
    /// `true` once the job has reached a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

/// Response from `POST /v1/advisor/query` — the job has been accepted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdvisorQueryStarted {
    pub query_id: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
    pub status: AdvisorJobStatus,
}

/// Status + result of an advisor job (`GET /v1/advisor/query/:id`). Extra
/// server fields (request echo, sink delivery, timestamps) are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdvisorQueryStatus {
    pub query_id: String,
    pub status: AdvisorJobStatus,
    #[serde(default)]
    pub result: Option<AdvisorOutput>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The advisor's answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdvisorOutput {
    pub summary: String,
    pub interpreter: AdvisorInterpreter,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdvisorInterpreter {
    pub summary: String,
    #[serde(default)]
    pub related_adrs: Vec<RelatedAdr>,
}

/// An ADR the advisor judged relevant to the query.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RelatedAdr {
    pub id: String,
    pub name: String,
    pub title: String,
    pub policy: String,
    pub instructions: String,
    pub scope: String,
    pub relevance_reason: String,
    pub confidence: f64,
    /// Fully-formed, environment-aware deep link back to this ADR in the
    /// Decisions UI. The server assembles it, so it is used verbatim and never
    /// reconstructed here. Nullable on the wire (an interpreter-layer result
    /// can carry `null` before the link is populated) and absent from older
    /// servers, so both cases deserialize to `None`.
    #[serde(default)]
    pub url: Option<String>,
}

/// Outcome of a single poll of `GET /v1/advisor/query/:id`.
#[derive(Debug, Clone, PartialEq)]
pub enum AdvisorPoll {
    /// `304 Not Modified` — the terminal result is unchanged from the ETag sent.
    NotModified,
    /// A transient `5xx` (e.g. the status row could not be loaded due to an
    /// infrastructure error) — retry the poll rather than failing the query.
    Retry { retry_after: Option<u64> },
    /// A status body, with the server's `Retry-After` (seconds) and `ETag` hints.
    Update {
        status: AdvisorQueryStatus,
        retry_after: Option<u64>,
        etag: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_full_match_response() {
        let json = r#"{
            "matched_adrs": [
                {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
                    "title": "Use App Router for all new pages",
                    "context": "Next.js 13+ introduced the App Router as the recommended routing paradigm. The Pages Router is maintained but not the direction Next.js is heading.",
                    "policies": [
                        "Use the App Router (app/ directory) for all new pages and routes",
                        "Do not create new files in the pages/ directory",
                        "Migrate existing Pages Router routes to App Router when modifying them"
                    ],
                    "instructions": [
                        "New routes go in app/ using the file-based routing convention (page.tsx, layout.tsx, etc.)",
                        "Use generateStaticParams for static generation instead of getStaticPaths"
                    ],
                    "category": {
                        "id": "cat-123",
                        "name": "Rendering Model",
                        "path": "UI/UX & Frontend Decisions > Rendering Model"
                    },
                    "applies_to": {
                        "languages": ["typescript", "javascript"],
                        "frameworks": ["nextjs"]
                    },
                    "matched_projects": ["apps/web"]
                }
            ],
            "metadata": {
                "total_matched": 32,
                "by_framework": {
                    "nextjs": 12,
                    "react": 8,
                    "fastify": 5,
                    "typescript": 15,
                    "general": 6
                },
                "deduplicated_count": 32
            }
        }"#;

        let response: MatchResponse = serde_json::from_str(json).unwrap();

        assert_eq!(response.matched_adrs.len(), 1);

        let adr = &response.matched_adrs[0];
        assert_eq!(adr.id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(adr.title, "Use App Router for all new pages");
        assert!(adr.context.is_some());
        assert!(adr
            .context
            .as_ref()
            .unwrap()
            .contains("App Router as the recommended"));
        assert_eq!(adr.policies.len(), 3);
        assert!(adr.policies[0].contains("App Router"));
        assert_eq!(adr.instructions.as_ref().unwrap().len(), 2);
        assert_eq!(adr.category.id, "cat-123");
        assert_eq!(adr.category.name, "Rendering Model");
        assert!(adr.category.path.contains("Frontend Decisions"));
        assert_eq!(adr.applies_to.languages, vec!["typescript", "javascript"]);
        assert_eq!(adr.applies_to.frameworks, vec!["nextjs"]);
        assert_eq!(adr.matched_projects, vec!["apps/web"]);

        assert_eq!(response.metadata.total_matched, 32);
        assert_eq!(response.metadata.deduplicated_count, 32);
        assert_eq!(response.metadata.by_framework.get("nextjs"), Some(&12));
        assert_eq!(response.metadata.by_framework.get("react"), Some(&8));
        assert_eq!(response.metadata.by_framework.get("fastify"), Some(&5));
        assert_eq!(response.metadata.by_framework.get("typescript"), Some(&15));
        assert_eq!(response.metadata.by_framework.get("general"), Some(&6));
        assert_eq!(response.metadata.by_framework.len(), 5);
    }

    #[test]
    fn test_serialize_match_request() {
        let request = MatchRequest {
            projects: vec![MatchProject {
                path: "apps/web".to_string(),
                name: "web-app".to_string(),
                languages: vec!["typescript".to_string(), "javascript".to_string()],
                frameworks: vec![
                    MatchFramework {
                        name: "nextjs".to_string(),
                        category: "frontend".to_string(),
                    },
                    MatchFramework {
                        name: "react".to_string(),
                        category: "frontend".to_string(),
                    },
                ],
                canonical_ir: None,
            }],
            options: Some(MatchOptions {
                include_general: Some(true),
                categories: None,
                exclude_categories: None,
                max_per_framework: Some(10),
            }),
        };

        let value = serde_json::to_value(&request).unwrap();

        // Verify projects array
        let projects = value["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["path"], "apps/web");
        assert_eq!(projects[0]["name"], "web-app");

        let languages = projects[0]["languages"].as_array().unwrap();
        assert_eq!(languages.len(), 2);
        assert_eq!(languages[0], "typescript");
        assert_eq!(languages[1], "javascript");

        let frameworks = projects[0]["frameworks"].as_array().unwrap();
        assert_eq!(frameworks.len(), 2);
        assert_eq!(frameworks[0]["name"], "nextjs");
        assert_eq!(frameworks[0]["category"], "frontend");
        assert_eq!(frameworks[1]["name"], "react");
        assert_eq!(frameworks[1]["category"], "frontend");

        // Verify options
        let options = &value["options"];
        assert_eq!(options["include_general"], true);
        assert_eq!(options["max_per_framework"], 10);
    }

    #[test]
    fn test_serialize_match_request_omits_none_options() {
        let request = MatchRequest {
            projects: vec![],
            options: None,
        };

        let value = serde_json::to_value(&request).unwrap();
        assert!(value.get("options").is_none());
    }

    #[test]
    fn test_deserialize_error_response() {
        let json = r#"{"error": {"code": "INVALID_REQUEST", "message": "At least one project is required", "details": null}}"#;

        let response: ApiErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.error.code, "INVALID_REQUEST");
        assert_eq!(response.error.message, "At least one project is required");
        assert!(response.error.details.is_none());
    }

    #[test]
    fn test_deserialize_response_with_optional_fields_absent() {
        let json = r#"{
            "matched_adrs": [
                {
                    "id": "abc-123",
                    "title": "Minimal ADR",
                    "context": null,
                    "policies": ["Do the thing"],
                    "instructions": null,
                    "category": {
                        "id": "cat-1",
                        "name": "General",
                        "path": "General"
                    },
                    "applies_to": {
                        "languages": ["rust"],
                        "frameworks": []
                    },
                    "matched_projects": []
                }
            ],
            "metadata": {
                "total_matched": 1,
                "by_framework": {},
                "deduplicated_count": 1
            }
        }"#;

        let response: MatchResponse = serde_json::from_str(json).unwrap();
        let adr = &response.matched_adrs[0];
        assert!(adr.context.is_none());
        assert!(adr.instructions.is_none());
        assert_eq!(adr.policies.len(), 1);
        assert_eq!(adr.matched_projects.len(), 0);
        assert_eq!(response.metadata.total_matched, 1);
    }

    #[test]
    fn test_round_trip_match_request() {
        let request = MatchRequest {
            projects: vec![MatchProject {
                path: "/my/project".to_string(),
                name: "my-project".to_string(),
                languages: vec!["rust".to_string()],
                frameworks: vec![MatchFramework {
                    name: "actix-web".to_string(),
                    category: "backend".to_string(),
                }],
                canonical_ir: None,
            }],
            options: Some(MatchOptions {
                include_general: Some(false),
                categories: Some(vec!["security".to_string()]),
                exclude_categories: Some(vec!["deprecated".to_string()]),
                max_per_framework: Some(5),
            }),
        };

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: MatchRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, deserialized);
    }

    #[test]
    fn test_deserialize_languages_response() {
        let json = r#"{
            "languages": [
                {
                    "id": "rust",
                    "display_name": "Rust",
                    "aliases": ["rs"]
                },
                {
                    "id": "typescript",
                    "display_name": "TypeScript",
                    "aliases": ["ts"]
                }
            ]
        }"#;

        let response: LanguagesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.languages.len(), 2);
        assert_eq!(response.languages[0].id, "rust");
        assert_eq!(response.languages[0].display_name, "Rust");
        assert_eq!(response.languages[0].aliases, vec!["rs"]);
        assert_eq!(response.languages[1].id, "typescript");
    }

    #[test]
    fn test_deserialize_frameworks_response() {
        let json = r#"{
            "frameworks": [
                {
                    "id": "nextjs",
                    "display_name": "Next.js",
                    "category": "frontend",
                    "languages": ["typescript", "javascript"]
                }
            ]
        }"#;

        let response: FrameworksResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.frameworks.len(), 1);
        assert_eq!(response.frameworks[0].id, "nextjs");
        assert_eq!(response.frameworks[0].display_name, "Next.js");
        assert_eq!(response.frameworks[0].category, "frontend");
        assert_eq!(
            response.frameworks[0].languages,
            vec!["typescript", "javascript"]
        );
    }

    #[test]
    fn test_deserialize_categories_response() {
        let json = r#"{
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
                            "children": []
                        }
                    ]
                }
            ]
        }"#;

        let response: CategoriesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.categories.len(), 1);
        assert_eq!(response.categories[0].id, "frontend");
        assert_eq!(response.categories[0].level, 0);
        assert_eq!(response.categories[0].children.len(), 1);
        assert_eq!(response.categories[0].children[0].id, "rendering");
        assert_eq!(response.categories[0].children[0].level, 1);
        assert!(response.categories[0].children[0].children.is_empty());
    }

    #[test]
    fn test_deserialize_health_response() {
        let json = r#"{"status": "ok", "version": "1.0.0"}"#;

        let response: HealthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.status, "ok");
        assert_eq!(response.version, "1.0.0");
    }

    #[test]
    fn test_deserialize_telemetry_request() {
        let json = r#"{
            "metrics": [
                {
                    "name": "adrs_matched",
                    "value": 42,
                    "tags": {
                        "project": "web-app",
                        "framework": "nextjs"
                    }
                }
            ]
        }"#;

        let request: TelemetryRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.metrics.len(), 1);
        assert_eq!(request.metrics[0].name, "adrs_matched");
        assert_eq!(request.metrics[0].value, 42);
        assert_eq!(
            request.metrics[0].tags.get("project"),
            Some(&"web-app".to_string())
        );
        assert_eq!(
            request.metrics[0].tags.get("framework"),
            Some(&"nextjs".to_string())
        );
    }

    #[test]
    fn test_is_v2_returns_false_for_v1_adr() {
        let adr = Adr {
            id: "adr-001".to_string(),
            title: "V1 ADR".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
                packages: vec![],
                version_range: None,
                facets: vec![],
            },
            matched_projects: vec![],
            schema_version: None,
            content_md: None,
            content_json: None,
            source: None,
        };
        assert!(!adr.is_v2());
    }

    #[test]
    fn test_is_v2_returns_true_when_schema_version_is_2() {
        let adr = Adr {
            id: "adr-002".to_string(),
            title: "V2 ADR".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
                packages: vec![],
                version_range: None,
                facets: vec![],
            },
            matched_projects: vec![],
            schema_version: Some(2),
            content_md: None,
            content_json: None,
            source: None,
        };
        assert!(adr.is_v2());
    }

    #[test]
    fn test_is_v2_returns_true_when_schema_version_is_greater_than_2() {
        let adr = Adr {
            id: "adr-003".to_string(),
            title: "V3 ADR".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
                packages: vec![],
                version_range: None,
                facets: vec![],
            },
            matched_projects: vec![],
            schema_version: Some(3),
            content_md: None,
            content_json: None,
            source: None,
        };
        assert!(adr.is_v2());
    }

    #[test]
    fn test_is_v2_returns_true_when_content_md_present_without_schema_version() {
        let adr = Adr {
            id: "adr-004".to_string(),
            title: "Content MD ADR".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
                packages: vec![],
                version_range: None,
                facets: vec![],
            },
            matched_projects: vec![],
            schema_version: None,
            content_md: Some("# My ADR\n\nContent here.".to_string()),
            content_json: None,
            source: None,
        };
        assert!(adr.is_v2());
    }

    #[test]
    fn test_is_v2_returns_false_when_schema_version_is_1() {
        let adr = Adr {
            id: "adr-005".to_string(),
            title: "Schema v1 ADR".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
                packages: vec![],
                version_range: None,
                facets: vec![],
            },
            matched_projects: vec![],
            schema_version: Some(1),
            content_md: None,
            content_json: None,
            source: None,
        };
        assert!(!adr.is_v2());
    }

    #[test]
    fn test_deserialize_v2_adr_with_all_fields() {
        let json = r##"{
            "matched_adrs": [
                {
                    "id": "v2-adr-001",
                    "title": "Use async/await consistently",
                    "context": "Modern Rust uses async/await for async code.",
                    "policies": ["Prefer async/await over callbacks"],
                    "instructions": null,
                    "category": {
                        "id": "cat-1",
                        "name": "Async Patterns",
                        "path": "Async Patterns"
                    },
                    "applies_to": {
                        "languages": ["rust"],
                        "frameworks": [],
                        "packages": ["tokio"],
                        "version_range": ">=1.0",
                        "facets": ["backend"]
                    },
                    "matched_projects": ["src"],
                    "schema_version": 2,
                    "content_md": "# Async Patterns",
                    "content_json": {"agent_documentation": "Use async/await for all async operations."},
                    "source": "https://example.com/adrs/v2-adr-001"
                }
            ],
            "metadata": {
                "total_matched": 1,
                "by_framework": {},
                "deduplicated_count": 1
            }
        }"##;

        let response: MatchResponse = serde_json::from_str(json).unwrap();
        let adr = &response.matched_adrs[0];
        assert_eq!(adr.id, "v2-adr-001");
        assert_eq!(adr.schema_version, Some(2));
        assert_eq!(adr.content_md.as_deref(), Some("# Async Patterns"));
        assert!(adr.content_json.is_some());
        assert_eq!(
            adr.source.as_deref(),
            Some("https://example.com/adrs/v2-adr-001")
        );
        assert_eq!(adr.applies_to.packages, vec!["tokio"]);
        assert_eq!(adr.applies_to.version_range.as_deref(), Some(">=1.0"));
        assert_eq!(adr.applies_to.facets, vec!["backend"]);
        assert!(adr.is_v2());
    }

    #[test]
    fn test_deserialize_v1_adr_missing_v2_fields_still_works() {
        // Existing V1 JSON without new optional fields must still deserialize.
        let json = r#"{
            "id": "v1-adr",
            "title": "Old ADR",
            "context": null,
            "policies": ["Do something"],
            "instructions": null,
            "category": {"id": "c1", "name": "General", "path": "General"},
            "applies_to": {"languages": ["rust"], "frameworks": []},
            "matched_projects": []
        }"#;

        let adr: Adr = serde_json::from_str(json).unwrap();
        assert_eq!(adr.id, "v1-adr");
        assert!(adr.schema_version.is_none());
        assert!(adr.content_md.is_none());
        assert!(adr.content_json.is_none());
        assert!(adr.source.is_none());
        assert!(adr.applies_to.packages.is_empty());
        assert!(adr.applies_to.version_range.is_none());
        assert!(adr.applies_to.facets.is_empty());
        assert!(!adr.is_v2());
    }

    #[test]
    fn test_error_response_with_details() {
        let json = r#"{
            "error": {
                "code": "VALIDATION_ERROR",
                "message": "Invalid input",
                "details": {"field": "projects", "reason": "empty array"}
            }
        }"#;

        let response: ApiErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.error.code, "VALIDATION_ERROR");
        assert!(response.error.details.is_some());
        let details = response.error.details.unwrap();
        assert_eq!(details["field"], "projects");
        assert_eq!(details["reason"], "empty array");
    }

    // --- Advisor query type tests ---

    #[test]
    fn test_advisor_surface_cli() {
        assert_eq!(AdvisorSurface::cli().kind, "cli");
    }

    #[test]
    fn test_advisor_job_status_is_terminal() {
        assert!(!AdvisorJobStatus::Pending.is_terminal());
        assert!(!AdvisorJobStatus::Running.is_terminal());
        assert!(AdvisorJobStatus::Succeeded.is_terminal());
        assert!(AdvisorJobStatus::Failed.is_terminal());
    }

    #[test]
    fn test_advisor_job_status_serde() {
        assert_eq!(
            serde_json::to_value(AdvisorJobStatus::Succeeded).unwrap(),
            serde_json::json!("succeeded")
        );
        let s: AdvisorJobStatus = serde_json::from_value(serde_json::json!("failed")).unwrap();
        assert_eq!(s, AdvisorJobStatus::Failed);
    }

    #[test]
    fn test_advisor_sink_none_serializes_with_type_tag() {
        assert_eq!(
            serde_json::to_value(AdvisorSink::None).unwrap(),
            serde_json::json!({ "type": "none" })
        );
    }

    #[test]
    fn test_advisor_query_request_serialization() {
        let req = AdvisorQueryRequest::new(
            "11111111-1111-1111-1111-111111111111".to_string(),
            None,
            "why?".to_string(),
            AdvisorSurface::cli(),
            AdvisorSink::None,
            None,
        );
        let v = serde_json::to_value(&req).unwrap();
        // Typed/versioned job envelope: type + version literals, query under data.
        assert_eq!(v["type"], "advisor_query");
        assert_eq!(v["version"], 1);
        assert_eq!(v["data"]["query"], "why?");
        assert!(v.get("query").is_none()); // not top-level anymore
        assert_eq!(v["surface"]["kind"], "cli");
        assert_eq!(v["sink"]["type"], "none");
        assert_eq!(v["org_id"], "11111111-1111-1111-1111-111111111111");
        // Absent optionals are omitted.
        assert!(v.get("repo_unique_id").is_none());
        assert!(v.get("idempotency_key").is_none());
    }

    #[test]
    fn test_advisor_query_request_includes_optionals_when_set() {
        let req = AdvisorQueryRequest::new(
            "o".to_string(),
            Some("22222222-2222-2222-2222-222222222222".to_string()),
            "q".to_string(),
            AdvisorSurface::cli(),
            AdvisorSink::None,
            Some("idem-1".to_string()),
        );
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["repo_unique_id"], "22222222-2222-2222-2222-222222222222");
        assert_eq!(v["idempotency_key"], "idem-1");
    }

    #[test]
    fn test_advisor_query_status_deserialize_ignores_extra_fields() {
        // The server echoes request/sink_delivery/timestamps; we ignore them.
        let json = r#"{
            "query_id": "q1",
            "workflow_id": "wf1",
            "status": "succeeded",
            "request": { "anything": true },
            "sink_delivery": null,
            "created_at": "2026-05-29T00:00:00Z",
            "result": {
                "summary": "top-level summary",
                "interpreter": {
                    "summary": "interp summary",
                    "related_adrs": [{
                        "id": "a1", "name": "n1", "title": "t1", "policy": "p1",
                        "instructions": "i1", "scope": "s1", "relevance_reason": "r1",
                        "confidence": 0.75,
                        "url": "https://app.example.com/decisions/r1?tab=active&decision=abc1234"
                    }]
                }
            },
            "error": null
        }"#;
        let status: AdvisorQueryStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.status, AdvisorJobStatus::Succeeded);
        let out = status.result.expect("result present");
        assert_eq!(out.summary, "top-level summary");
        assert_eq!(out.interpreter.related_adrs.len(), 1);
        assert_eq!(out.interpreter.related_adrs[0].confidence, 0.75);
        // The server's deep link is captured verbatim.
        assert_eq!(
            out.interpreter.related_adrs[0].url.as_deref(),
            Some("https://app.example.com/decisions/r1?tab=active&decision=abc1234")
        );
    }

    #[test]
    fn test_related_adr_url_absent_or_null_is_none() {
        // `url` absent (older server) and `url: null` (interpreter-layer result
        // before the link is populated) both deserialize to None.
        let json = r#"{
            "summary": "s",
            "interpreter": {
                "summary": "i",
                "related_adrs": [
                    { "id": "a1", "name": "n1", "title": "t1", "policy": "p1",
                      "instructions": "i1", "scope": "s1", "relevance_reason": "r1",
                      "confidence": 0.5 },
                    { "id": "a2", "name": "n2", "title": "t2", "policy": "p2",
                      "instructions": "i2", "scope": "s2", "relevance_reason": "r2",
                      "confidence": 0.6, "url": null }
                ]
            }
        }"#;
        let out: AdvisorOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.interpreter.related_adrs[0].url, None);
        assert_eq!(out.interpreter.related_adrs[1].url, None);
    }

    #[test]
    fn test_connected_repos_response_deserializes_all_fields() {
        let json = r#"{
            "repositories": [
                {
                    "repo_unique_id": "33333333-3333-3333-3333-333333333333",
                    "name": "actual-cli",
                    "external_owner": "actual-software",
                    "url": "https://github.com/actual-software/actual-cli"
                }
            ]
        }"#;
        let resp: GetConnectedReposResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.repositories.len(), 1);
        let repo = &resp.repositories[0];
        assert_eq!(repo.repo_unique_id, "33333333-3333-3333-3333-333333333333");
        assert_eq!(repo.name, "actual-cli");
        assert_eq!(repo.external_owner, "actual-software");
        assert_eq!(repo.url, "https://github.com/actual-software/actual-cli");
    }

    #[test]
    fn test_connected_repos_response_missing_repositories_defaults_empty() {
        // A body without `repositories` deserializes to an empty list (the
        // `#[serde(default)]` path), not an error.
        let resp: GetConnectedReposResponse = serde_json::from_str("{}").unwrap();
        assert!(resp.repositories.is_empty());
    }

    #[test]
    fn test_connected_repos_error_body_reads_message() {
        let body: ConnectedReposErrorBody =
            serde_json::from_str(r#"{"error":"not_found","message":"organization not found"}"#)
                .unwrap();
        assert_eq!(body.message, "organization not found");
    }

    #[test]
    fn test_connected_repos_error_body_missing_message_defaults_empty() {
        // Absent `message` (and an ignored, unknown `error` code) defaults to "".
        let body: ConnectedReposErrorBody = serde_json::from_str("{}").unwrap();
        assert_eq!(body.message, "");
    }
}
