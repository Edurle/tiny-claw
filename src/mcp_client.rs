use crate::types::*;
use anyhow::{Context, Result};
use eventsource_stream::Eventsource;
use futures::StreamExt;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

pub struct McpConnection {
    pub name: String,
    base_url: String,
    post_endpoint: String,
    session_id: Mutex<Option<String>>,
    next_id: AtomicU64,
    client: reqwest::Client,
}

impl McpConnection {
    /// Connect to an MCP server via SSE.
    /// Performs the legacy SSE transport handshake:
    /// 1. GET the URL with Accept: text/event-stream
    /// 2. Wait for "endpoint" event containing the POST endpoint
    /// 3. Return the connection ready for JSON-RPC calls
    pub async fn connect(name: &str, url: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        let base_url = url.trim_end_matches('/').to_string();

        tracing::info!("Connecting to MCP server '{}' at {}", name, base_url);

        // Try legacy SSE transport: GET the URL to receive SSE stream
        let response = client
            .get(&base_url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .with_context(|| format!("Failed to connect to MCP server '{}'", name))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "MCP server '{}' returned status {}",
                name,
                response.status()
            );
        }

        // Parse SSE stream to find the "endpoint" event
        let mut stream = response.bytes_stream().eventsource();
        let mut post_endpoint = String::new();
        let mut found_endpoint = false;

        while let Some(event) = stream.next().await {
            match event {
                Ok(sse_event) => {
                    if sse_event.event == "endpoint" {
                        let endpoint_path = sse_event.data.trim().to_string();
                        if endpoint_path.starts_with("http") {
                            post_endpoint = endpoint_path;
                        } else {
                            // Relative path: combine with base URL
                            post_endpoint = format!("{}{}", base_url, endpoint_path);
                        }
                        tracing::info!(
                            "MCP server '{}' endpoint: {}",
                            name,
                            post_endpoint
                        );
                        found_endpoint = true;
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("SSE parse error for '{}': {}", name, e);
                    break;
                }
            }
        }

        if !found_endpoint {
            anyhow::bail!(
                "MCP server '{}' did not send an 'endpoint' SSE event",
                name
            );
        }

        Ok(Self {
            name: name.to_string(),
            base_url,
            post_endpoint,
            session_id: Mutex::new(None),
            next_id: AtomicU64::new(1),
            client,
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and wait for the response via SSE.
    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let body = serde_json::to_value(&request)
            .with_context(|| "Failed to serialize JSON-RPC request")?;

        // Send the request via POST
        let mut req_builder = self
            .client
            .post(&self.post_endpoint)
            .header("Content-Type", "application/json")
            .json(&body);

        let session = self.session_id.lock().await;
        if let Some(ref sid) = *session {
            req_builder = req_builder.header("Mcp-Session-Id", sid.as_str());
        }
        drop(session);

        let post_response = req_builder
            .send()
            .await
            .with_context(|| format!("Failed to send request to MCP server '{}'", self.name))?;

        let status = post_response.status();
        if !status.is_success() {
            let text = post_response.text().await.unwrap_or_default();
            anyhow::bail!(
                "MCP server '{}' error ({}): {}",
                self.name,
                status,
                text.chars().take(300).collect::<String>()
            );
        }

        // Check if response is JSON directly (Streamable HTTP) or SSE (legacy)
        let content_type = post_response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("application/json") {
            // Direct JSON response (Streamable HTTP)
            let rpc_response: JsonRpcResponse = post_response
                .json()
                .await
                .with_context(|| "Failed to parse MCP JSON response")?;
            return Ok(rpc_response);
        }

        // SSE response: parse events looking for our response
        let request_id = request.id;
        let mut stream = post_response.bytes_stream().eventsource();

        while let Some(event) = stream.next().await {
            match event {
                Ok(sse_event) => {
                    if let Ok(rpc_response) = serde_json::from_str::<JsonRpcResponse>(&sse_event.data)
                    {
                        if rpc_response.id == Some(request_id) {
                            if let Some(err) = &rpc_response.error {
                                anyhow::bail!(
                                    "MCP server '{}' error: [{}] {}",
                                    self.name,
                                    err.code,
                                    err.message
                                );
                            }
                            return Ok(rpc_response);
                        }
                    }
                }
                Err(e) => {
                    anyhow::bail!("SSE stream error for '{}': {}", self.name, e);
                }
            }
        }

        anyhow::bail!(
            "MCP server '{}' closed SSE stream without response for request {}",
            self.name,
            request_id
        );
    }

    /// Perform the MCP initialize handshake.
    pub async fn initialize(&self) -> Result<()> {
        let params = InitializeParams {
            protocolVersion: "2024-11-05",
            capabilities: serde_json::json!({}),
            clientInfo: ClientInfo {
                name: "tiny-claw".to_string(),
                version: "0.1.0".to_string(),
            },
        };

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: self.next_id(),
            method: "initialize".to_string(),
            params: Some(serde_json::to_value(params)?),
        };

        let response = self.send_request(request).await?;
        tracing::info!(
            "MCP server '{}' initialized: {:?}",
            self.name,
            response.result
        );

        // Send initialized notification (no id = notification)
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });

        let mut req_builder = self
            .client
            .post(&self.post_endpoint)
            .header("Content-Type", "application/json")
            .json(&notification);

        let session = self.session_id.lock().await;
        if let Some(ref sid) = *session {
            req_builder = req_builder.header("Mcp-Session-Id", sid.as_str());
        }

        let _ = req_builder.send().await;

        Ok(())
    }

    /// List all tools from the server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: self.next_id(),
            method: "tools/list".to_string(),
            params: Some(serde_json::json!({})),
        };

        let response = self.send_request(request).await?;

        let result = response
            .result
            .with_context(|| format!("MCP server '{}' returned no result for tools/list", self.name))?;

        #[derive(serde::Deserialize)]
        struct ToolsListResult {
            #[serde(default)]
            tools: Vec<McpTool>,
        }

        let tools_result: ToolsListResult = serde_json::from_value(result)
            .with_context(|| "Failed to parse tools/list result")?;

        tracing::info!(
            "MCP server '{}' has {} tools",
            self.name,
            tools_result.tools.len()
        );

        Ok(tools_result.tools)
    }

    /// Call a tool by name with arguments.
    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<McpToolResult> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: self.next_id(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": name,
                "arguments": args,
            })),
        };

        let response = self.send_request(request).await?;

        let result = response.result.with_context(|| {
            format!(
                "MCP server '{}' returned no result for tools/call {}",
                self.name, name
            )
        })?;

        let tool_result: McpToolResult = serde_json::from_value(result)
            .with_context(|| "Failed to parse tools/call result")?;

        Ok(tool_result)
    }
}
