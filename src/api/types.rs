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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Adr {
    pub id: String,
    pub title: String,
    pub context: Option<String>,
    pub policies: Vec<String>,
    pub instructions: Option<Vec<String>>,
    pub category: AdrCategory,
    pub applies_to: AppliesTo,
    pub matched_projects: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdrCategory {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliesTo {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
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
}
