use crate::config::TrackerConfig;
use crate::tracker::execute_linear_graphql;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use tracing::debug;

/// MCP protocol version supported by this server.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

// ── JSON-RPC types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

// ── MCP protocol handlers ───────────────────────────────────────────

fn handle_initialize() -> Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "symphony-linear",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn handle_tools_list() -> Value {
    serde_json::json!({
        "tools": [{
            "name": "linear_graphql",
            "description": "Execute a GraphQL query against the Linear API. Use this to read and write Linear issues, comments, projects, and other data.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The GraphQL query or mutation to execute"
                    },
                    "variables": {
                        "type": "object",
                        "description": "Optional variables for the GraphQL query"
                    }
                },
                "required": ["query"]
            }
        }]
    })
}

// ── Tool execution ──────────────────────────────────────────────────

async fn handle_tools_call(params: &Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

    match name {
        "linear_graphql" => execute_linear_graphql_tool(&arguments).await,
        _ => tool_error(&format!("unknown tool: {name}")),
    }
}

async fn execute_linear_graphql_tool(arguments: &Value) -> Value {
    let query = match arguments.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return tool_error("missing required parameter: query"),
    };
    let variables = arguments.get("variables");

    // Build TrackerConfig from environment
    let config = match build_tracker_config_from_env() {
        Ok(c) => c,
        Err(msg) => return tool_error(&msg),
    };

    run_graphql_query(&config, query, variables).await
}

/// Build a `TrackerConfig` from environment variables.
///
/// Returns `Err` with a user-facing message if `LINEAR_API_KEY` is missing.
fn build_tracker_config_from_env() -> std::result::Result<TrackerConfig, String> {
    let api_key = std::env::var("LINEAR_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return Err("LINEAR_API_KEY environment variable not set".to_string());
    }

    let endpoint = std::env::var("LINEAR_ENDPOINT")
        .unwrap_or_else(|_| "https://api.linear.app/graphql".to_string());

    Ok(TrackerConfig {
        kind: "linear".to_string(),
        endpoint,
        api_key,
        team_key: std::env::var("LINEAR_TEAM_KEY").unwrap_or_default(),
        active_states: vec![],
        terminal_states: vec![],
    })
}

/// Execute a GraphQL query using the given config and return an MCP tool result.
async fn run_graphql_query(
    config: &TrackerConfig,
    query: &str,
    variables: Option<&Value>,
) -> Value {
    match execute_linear_graphql(config, query, variables).await {
        Ok(data) => tool_success(&serde_json::to_string(&data).unwrap_or_default()),
        Err(e) => tool_error(&e),
    }
}

fn tool_success(text: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn tool_error(message: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true
    })
}

// ── Request dispatch ────────────────────────────────────────────────

async fn handle_request(request: &JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id.clone().unwrap_or(Value::Null);

    match request.method.as_str() {
        "initialize" => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(handle_initialize()),
            error: None,
        },
        "tools/list" => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(handle_tools_list()),
            error: None,
        },
        "tools/call" => {
            let params = request.params.as_ref().cloned().unwrap_or(Value::Null);
            let result = handle_tools_call(&params).await;
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(result),
                error: None,
            }
        }
        _ => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("method not found: {}", request.method),
                data: None,
            }),
        },
    }
}

// ── Server loop ─────────────────────────────────────────────────────

/// Run the MCP server, reading JSON-RPC requests from `reader` and writing
/// responses to `writer`.
///
/// Accepts generic `BufRead`/`Write` so tests can use in-memory buffers.
pub async fn run_mcp_server<R: BufRead, W: Write>(
    reader: R,
    mut writer: W,
) -> std::result::Result<(), String> {
    for line in reader.lines() {
        let line = line.map_err(|e| format!("stdin read error: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: Value::Null,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("parse error: {e}"),
                        data: None,
                    }),
                };
                let json = serde_json::to_string(&resp).unwrap_or_default();
                writeln!(writer, "{json}").map_err(|e| format!("write error: {e}"))?;
                writer.flush().map_err(|e| format!("flush error: {e}"))?;
                continue;
            }
        };

        // Notifications (no id) don't get responses
        if request.id.is_none() {
            debug!(method = %request.method, "received notification, no response needed");
            continue;
        }

        let response = handle_request(&request).await;
        let json = serde_json::to_string(&response).unwrap_or_default();
        writeln!(writer, "{json}").map_err(|e| format!("write error: {e}"))?;
        writer.flush().map_err(|e| format!("flush error: {e}"))?;
    }

    Ok(())
}

// ── MCP config generation ───────────────────────────────────────────

/// Generate the JSON content for an MCP config file that launches this binary
/// as an MCP server.
pub fn generate_mcp_config(symphony_binary: &Path, env_vars: &HashMap<String, String>) -> String {
    let binary_path = symphony_binary.display().to_string();
    let mut env_obj = serde_json::Map::new();

    if let Some(api_key) = env_vars.get("LINEAR_API_KEY") {
        env_obj.insert("LINEAR_API_KEY".to_string(), Value::String(api_key.clone()));
    }
    if let Some(team_key) = env_vars.get("LINEAR_TEAM_KEY") {
        env_obj.insert(
            "LINEAR_TEAM_KEY".to_string(),
            Value::String(team_key.clone()),
        );
    }

    let config = serde_json::json!({
        "mcpServers": {
            "symphony-linear": {
                "command": binary_path,
                "args": ["--mcp-server"],
                "env": env_obj
            }
        }
    });

    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Write the MCP config file to the workspace directory and return the path.
pub fn write_mcp_config(
    workspace_path: &Path,
    symphony_binary: &Path,
    env_vars: &HashMap<String, String>,
) -> std::io::Result<std::path::PathBuf> {
    let config_content = generate_mcp_config(symphony_binary, env_vars);
    let config_path = workspace_path.join(".mcp-config.json");
    std::fs::write(&config_path, config_content)?;
    Ok(config_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── JSON-RPC parsing ────────────────────────────────────────────

    #[tokio::test]
    async fn test_server_handles_valid_request() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let reader = Cursor::new(format!("{input}\n"));
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_line = String::from_utf8(output).unwrap();
        let response: Value = serde_json::from_str(response_line.trim()).unwrap();
        assert_eq!(response["id"], 1);
        assert!(response["result"]["protocolVersion"].is_string());
    }

    #[tokio::test]
    async fn test_server_handles_invalid_json() {
        let input = "this is not json\n";
        let reader = Cursor::new(input.to_string());
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_line = String::from_utf8(output).unwrap();
        let response: Value = serde_json::from_str(response_line.trim()).unwrap();
        assert_eq!(response["error"]["code"], -32700);
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("parse error"));
    }

    #[tokio::test]
    async fn test_server_skips_notification_no_response() {
        // A notification has no `id` field — the server should not respond
        let input = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let reader = Cursor::new(format!("{input}\n"));
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_text = String::from_utf8(output).unwrap();
        assert!(
            response_text.is_empty(),
            "notifications should not produce a response"
        );
    }

    #[tokio::test]
    async fn test_server_skips_empty_lines() {
        let input = "\n\n\n";
        let reader = Cursor::new(input.to_string());
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_text = String::from_utf8(output).unwrap();
        assert!(response_text.is_empty(), "empty lines produce no output");
    }

    #[tokio::test]
    async fn test_server_handles_multiple_messages() {
        let input = format!(
            "{}\n{}\n",
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#
        );
        let reader = Cursor::new(input);
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = response_text.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "should have two response lines");

        let resp1: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(resp1["id"], 1);

        let resp2: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(resp2["id"], 2);
    }

    // ── initialize handler ──────────────────────────────────────────

    #[test]
    fn test_handle_initialize_returns_protocol_version() {
        let result = handle_initialize();
        assert_eq!(result["protocolVersion"], MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn test_handle_initialize_returns_capabilities() {
        let result = handle_initialize();
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn test_handle_initialize_returns_server_info() {
        let result = handle_initialize();
        assert_eq!(result["serverInfo"]["name"], "symphony-linear");
        assert!(result["serverInfo"]["version"].is_string());
    }

    // ── tools/list handler ──────────────────────────────────────────

    #[test]
    fn test_handle_tools_list_returns_linear_graphql_tool() {
        let result = handle_tools_list();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "linear_graphql");
    }

    #[test]
    fn test_handle_tools_list_tool_has_input_schema() {
        let result = handle_tools_list();
        let tool = &result["tools"][0];
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&Value::String("query".to_string())));
    }

    #[test]
    fn test_handle_tools_list_tool_has_description() {
        let result = handle_tools_list();
        let tool = &result["tools"][0];
        assert!(tool["description"].as_str().unwrap().contains("GraphQL"));
    }

    #[test]
    fn test_handle_tools_list_tool_has_variables_property() {
        let result = handle_tools_list();
        let tool = &result["tools"][0];
        let schema = &tool["inputSchema"];
        assert!(schema["properties"]["variables"].is_object());
        assert_eq!(schema["properties"]["variables"]["type"], "object");
    }

    // ── tools/call - linear_graphql ─────────────────────────────────

    #[tokio::test]
    async fn test_tools_call_linear_graphql_missing_query() {
        let params = serde_json::json!({
            "name": "linear_graphql",
            "arguments": {}
        });
        let result = handle_tools_call(&params).await;
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing required parameter: query"));
    }

    #[tokio::test]
    async fn test_tools_call_linear_graphql_empty_api_key() {
        // Temporarily clear the env var to test the error path
        let original = std::env::var("LINEAR_API_KEY").ok();
        std::env::remove_var("LINEAR_API_KEY");

        let params = serde_json::json!({
            "name": "linear_graphql",
            "arguments": { "query": "{ viewer { id } }" }
        });
        let result = handle_tools_call(&params).await;

        // Restore
        if let Some(val) = original {
            std::env::set_var("LINEAR_API_KEY", val);
        }

        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("LINEAR_API_KEY"));
    }

    #[tokio::test]
    async fn test_tools_call_linear_graphql_success_with_mock() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_body(r#"{"data":{"viewer":{"id":"u1"}}}"#)
            .create_async()
            .await;

        // We need to call execute_linear_graphql_tool but it reads
        // LINEAR_API_KEY from env. Instead, test via the lower-level path.
        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: server.url(),
            api_key: "test-key".to_string(),
            team_key: "test-team".to_string(),
            active_states: vec![],
            terminal_states: vec![],
        };

        let result = execute_linear_graphql(&config, "{ viewer { id } }", None).await;
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data["data"]["viewer"]["id"], "u1");

        // Now test tool_success formatting
        let tool_result = tool_success(&serde_json::to_string(&data).unwrap());
        assert!(tool_result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("u1"));
        assert!(tool_result.get("isError").is_none());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_tools_call_linear_graphql_with_variables() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_body(r#"{"data":{"issue":{"id":"i1"}}}"#)
            .create_async()
            .await;

        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: server.url(),
            api_key: "test-key".to_string(),
            team_key: "test-team".to_string(),
            active_states: vec![],
            terminal_states: vec![],
        };

        let vars = serde_json::json!({"id": "i1"});
        let result = execute_linear_graphql(
            &config,
            "query($id: ID!) { issue(id: $id) { id } }",
            Some(&vars),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["data"]["issue"]["id"], "i1");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_tools_call_linear_graphql_api_error() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: server.url(),
            api_key: "test-key".to_string(),
            team_key: "test-team".to_string(),
            active_states: vec![],
            terminal_states: vec![],
        };

        let result = execute_linear_graphql(&config, "{ viewer { id } }", None).await;
        assert!(result.is_err());

        // Test tool_error formatting with the error
        let err_msg = result.unwrap_err();
        let tool_result = tool_error(&err_msg);
        assert_eq!(tool_result["isError"], true);
        assert!(tool_result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("HTTP 500"));
    }

    // ── tools/call - unknown tool ───────────────────────────────────

    #[tokio::test]
    async fn test_tools_call_unknown_tool() {
        let params = serde_json::json!({
            "name": "nonexistent_tool",
            "arguments": {}
        });
        let result = handle_tools_call(&params).await;
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool: nonexistent_tool"));
    }

    #[tokio::test]
    async fn test_tools_call_missing_name() {
        let params = serde_json::json!({
            "arguments": {}
        });
        let result = handle_tools_call(&params).await;
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool: "));
    }

    // ── Unknown method ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_handle_request_unknown_method() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(serde_json::Number::from(1))),
            method: "some/unknown".to_string(),
            params: None,
        };
        let response = handle_request(&request).await;
        assert!(response.error.is_some());
        let err = response.error.unwrap();
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("method not found: some/unknown"));
    }

    // ── tool_success and tool_error helpers ──────────────────────────

    #[test]
    fn test_tool_success_format() {
        let result = tool_success("hello world");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "hello world");
        assert!(result.get("isError").is_none());
    }

    #[test]
    fn test_tool_error_format() {
        let result = tool_error("something went wrong");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "something went wrong");
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn test_tool_success_empty_text() {
        let result = tool_success("");
        assert_eq!(result["content"][0]["text"], "");
        assert!(result.get("isError").is_none());
    }

    #[test]
    fn test_tool_error_empty_message() {
        let result = tool_error("");
        assert_eq!(result["content"][0]["text"], "");
        assert_eq!(result["isError"], true);
    }

    // ── MCP config generation ───────────────────────────────────────

    #[test]
    fn test_generate_mcp_config_with_both_env_vars() {
        let mut env_vars = HashMap::new();
        env_vars.insert("LINEAR_API_KEY".to_string(), "lin_api_test".to_string());
        env_vars.insert("LINEAR_TEAM_KEY".to_string(), "my-team".to_string());

        let config = generate_mcp_config(Path::new("/usr/bin/symphony"), &env_vars);
        let parsed: Value = serde_json::from_str(&config).unwrap();

        assert_eq!(
            parsed["mcpServers"]["symphony-linear"]["command"],
            "/usr/bin/symphony"
        );
        assert_eq!(
            parsed["mcpServers"]["symphony-linear"]["args"][0],
            "--mcp-server"
        );
        assert_eq!(
            parsed["mcpServers"]["symphony-linear"]["env"]["LINEAR_API_KEY"],
            "lin_api_test"
        );
        assert_eq!(
            parsed["mcpServers"]["symphony-linear"]["env"]["LINEAR_TEAM_KEY"],
            "my-team"
        );
    }

    #[test]
    fn test_generate_mcp_config_with_api_key_only() {
        let mut env_vars = HashMap::new();
        env_vars.insert("LINEAR_API_KEY".to_string(), "lin_api_test".to_string());

        let config = generate_mcp_config(Path::new("/usr/bin/symphony"), &env_vars);
        let parsed: Value = serde_json::from_str(&config).unwrap();

        assert_eq!(
            parsed["mcpServers"]["symphony-linear"]["env"]["LINEAR_API_KEY"],
            "lin_api_test"
        );
        assert!(parsed["mcpServers"]["symphony-linear"]["env"]
            .get("LINEAR_TEAM_KEY")
            .is_none());
    }

    #[test]
    fn test_generate_mcp_config_with_no_env_vars() {
        let env_vars = HashMap::new();

        let config = generate_mcp_config(Path::new("/usr/bin/symphony"), &env_vars);
        let parsed: Value = serde_json::from_str(&config).unwrap();

        let env_obj = parsed["mcpServers"]["symphony-linear"]["env"]
            .as_object()
            .unwrap();
        assert!(env_obj.is_empty());
    }

    #[test]
    fn test_generate_mcp_config_ignores_unrelated_env_vars() {
        let mut env_vars = HashMap::new();
        env_vars.insert("LINEAR_API_KEY".to_string(), "key".to_string());
        env_vars.insert("UNRELATED_VAR".to_string(), "value".to_string());

        let config = generate_mcp_config(Path::new("/usr/bin/symphony"), &env_vars);
        let parsed: Value = serde_json::from_str(&config).unwrap();

        let env_obj = parsed["mcpServers"]["symphony-linear"]["env"]
            .as_object()
            .unwrap();
        assert_eq!(env_obj.len(), 1);
        assert!(env_obj.get("UNRELATED_VAR").is_none());
    }

    // ── MCP config file writing ─────────────────────────────────────

    #[test]
    fn test_write_mcp_config_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut env_vars = HashMap::new();
        env_vars.insert("LINEAR_API_KEY".to_string(), "test-key".to_string());

        let result = write_mcp_config(dir.path(), Path::new("/usr/bin/symphony"), &env_vars);
        assert!(result.is_ok());

        let config_path = result.unwrap();
        assert!(config_path.exists());
        assert_eq!(config_path.file_name().unwrap(), ".mcp-config.json");

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["mcpServers"]["symphony-linear"]["command"],
            "/usr/bin/symphony"
        );
    }

    #[test]
    fn test_write_mcp_config_returns_correct_path() {
        let dir = tempfile::tempdir().unwrap();
        let env_vars = HashMap::new();

        let result = write_mcp_config(dir.path(), Path::new("/bin/sym"), &env_vars);
        assert!(result.is_ok());
        let path = result.unwrap();
        assert_eq!(path, dir.path().join(".mcp-config.json"));
    }

    // ── handle_request dispatch ─────────────────────────────────────

    #[tokio::test]
    async fn test_handle_request_initialize() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(serde_json::Number::from(42))),
            method: "initialize".to_string(),
            params: None,
        };
        let response = handle_request(&request).await;
        assert!(response.error.is_none());
        assert_eq!(response.id, 42);
        let result = response.result.unwrap();
        assert_eq!(result["protocolVersion"], MCP_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn test_handle_request_tools_list() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::String("req-1".to_string())),
            method: "tools/list".to_string(),
            params: None,
        };
        let response = handle_request(&request).await;
        assert!(response.error.is_none());
        assert_eq!(response.id, "req-1");
        let result = response.result.unwrap();
        assert!(result["tools"].is_array());
    }

    #[tokio::test]
    async fn test_handle_request_tools_call_dispatches() {
        // Test that tools/call routes to handle_tools_call
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(serde_json::Number::from(5))),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "nonexistent",
                "arguments": {}
            })),
        };
        let response = handle_request(&request).await;
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        // The tool error is embedded in the result, not in the JSON-RPC error
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn test_handle_request_tools_call_no_params() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(serde_json::Number::from(6))),
            method: "tools/call".to_string(),
            params: None,
        };
        let response = handle_request(&request).await;
        assert!(response.error.is_none());
        // With null params and no name, we get "unknown tool: "
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true);
    }

    // ── Server loop edge cases ──────────────────────────────────────

    #[tokio::test]
    async fn test_server_interleaved_notifications_and_requests() {
        let input = format!(
            "{}\n{}\n{}\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#,
        );
        let reader = Cursor::new(input);
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = response_text.trim().split('\n').collect();
        assert_eq!(
            lines.len(),
            1,
            "only the request with id should produce a response"
        );

        let resp: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(resp["id"], 1);
    }

    #[tokio::test]
    async fn test_server_parse_error_then_valid_request() {
        let input = format!(
            "{}\n{}\n",
            "not valid json", r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}"#
        );
        let reader = Cursor::new(input);
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = response_text.trim().split('\n').collect();
        assert_eq!(lines.len(), 2);

        // First line: parse error
        let resp1: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(resp1["error"]["code"], -32700);

        // Second line: valid response
        let resp2: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(resp2["id"], 2);
        assert!(resp2["result"].is_object());
    }

    #[tokio::test]
    async fn test_server_empty_input() {
        let reader = Cursor::new(String::new());
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_text = String::from_utf8(output).unwrap();
        assert!(response_text.is_empty());
    }

    // ── JsonRpcResponse serialization ───────────────────────────────

    #[test]
    fn test_json_rpc_response_skips_none_fields() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Value::Number(serde_json::Number::from(1)),
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("error"));

        let resp_err = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Value::Number(serde_json::Number::from(1)),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: "not found".to_string(),
                data: None,
            }),
        };
        let json_err = serde_json::to_string(&resp_err).unwrap();
        assert!(!json_err.contains("result"));
        assert!(!json_err.contains("\"data\""));
    }

    #[test]
    fn test_json_rpc_error_with_data() {
        let err = JsonRpcError {
            code: -32600,
            message: "invalid request".to_string(),
            data: Some(serde_json::json!({"detail": "missing method"})),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"data\""));
        assert!(json.contains("missing method"));
    }

    // ── execute_linear_graphql_tool with null arguments ─────────────

    #[tokio::test]
    async fn test_execute_linear_graphql_tool_null_arguments() {
        let result = execute_linear_graphql_tool(&Value::Null).await;
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing required parameter"));
    }

    // ── Full integration: tools/call via server loop ────────────────

    #[tokio::test]
    async fn test_server_tools_call_missing_query_via_loop() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"linear_graphql","arguments":{}}}"#;
        let reader = Cursor::new(format!("{input}\n"));
        let mut output = Vec::new();

        run_mcp_server(reader, &mut output).await.unwrap();

        let response_line = String::from_utf8(output).unwrap();
        let resp: Value = serde_json::from_str(response_line.trim()).unwrap();
        assert_eq!(resp["id"], 1);
        let result = &resp["result"];
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing required parameter"));
    }

    // ── run_graphql_query (bypasses env vars) ─────────────────────

    /// Test `run_graphql_query` success path with a mock server.
    #[tokio::test]
    async fn test_run_graphql_query_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_body(r#"{"data":{"viewer":{"id":"u-direct"}}}"#)
            .create_async()
            .await;

        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: server.url(),
            api_key: "test-key".to_string(),
            team_key: "test-team".to_string(),
            active_states: vec![],
            terminal_states: vec![],
        };

        let result = run_graphql_query(&config, "{ viewer { id } }", None).await;

        assert!(
            result.get("isError").is_none(),
            "expected success but got error: {result:?}"
        );
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("u-direct"));

        mock.assert_async().await;
    }

    /// Test `run_graphql_query` error path with a mock server.
    #[tokio::test]
    async fn test_run_graphql_query_error() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: server.url(),
            api_key: "bad-key".to_string(),
            team_key: "test-team".to_string(),
            active_states: vec![],
            terminal_states: vec![],
        };

        let result = run_graphql_query(&config, "{ viewer { id } }", None).await;
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("HTTP 401"));
    }

    /// Test `run_graphql_query` with variables.
    #[tokio::test]
    async fn test_run_graphql_query_with_variables() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_body(r#"{"data":{"issue":{"id":"iss-1"}}}"#)
            .create_async()
            .await;

        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: server.url(),
            api_key: "test-key".to_string(),
            team_key: "test-team".to_string(),
            active_states: vec![],
            terminal_states: vec![],
        };

        let vars = serde_json::json!({"id": "iss-1"});
        let result = run_graphql_query(
            &config,
            "query($id: ID!) { issue(id: $id) { id } }",
            Some(&vars),
        )
        .await;

        assert!(result.get("isError").is_none());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("iss-1"));

        mock.assert_async().await;
    }
}
