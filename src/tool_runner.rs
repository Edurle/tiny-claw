use crate::config::McpServerConfig;
use crate::mcp_client::McpConnection;
use crate::types::*;
use anyhow::Result;
use std::collections::HashMap;

pub struct ToolRegistry {
    tool_to_server: HashMap<String, String>,
    connections: HashMap<String, McpConnection>,
    openai_tools: Vec<ToolDefinition>,
}

impl ToolRegistry {
    /// Connect to all MCP servers, discover tools, build routing map.
    pub async fn build(server_configs: &[McpServerConfig]) -> Result<Self> {
        let mut tool_to_server = HashMap::new();
        let mut connections = HashMap::new();
        let mut openai_tools = Vec::new();

        for config in server_configs {
            match McpConnection::connect(&config.name, &config.url).await {
                Ok(conn) => {
                    // Initialize the connection
                    if let Err(e) = conn.initialize().await {
                        tracing::warn!(
                            "Failed to initialize MCP server '{}': {}",
                            config.name,
                            e
                        );
                        continue;
                    }

                    // Discover tools
                    match conn.list_tools().await {
                        Ok(tools) => {
                            for tool in &tools {
                                tool_to_server
                                    .insert(tool.name.clone(), config.name.clone());
                                openai_tools.push(mcp_tool_to_openai(tool));
                            }
                            connections.insert(config.name.clone(), conn);
                            tracing::info!(
                                "Server '{}': discovered {} tools",
                                config.name,
                                tools.len()
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to list tools from MCP server '{}': {}",
                                config.name,
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Could not connect to MCP server '{}' at {}: {}",
                        config.name,
                        config.url,
                        e
                    );
                }
            }
        }

        Ok(Self {
            tool_to_server,
            connections,
            openai_tools,
        })
    }

    /// Get all tools in OpenAI format.
    pub fn get_openai_tools(&self) -> Vec<ToolDefinition> {
        self.openai_tools.clone()
    }

    /// Execute a tool call by routing to the correct MCP server.
    /// Always returns Ok — errors are returned as text for the LLM to handle.
    pub async fn execute_tool_call(&self, tool_call: &ToolCall) -> Result<String> {
        // Route to server
        let server_name = match self.tool_to_server.get(&tool_call.function.name) {
            Some(s) => s,
            None => {
                return Ok(format!(
                    "Error: tool '{}' not found in any MCP server",
                    tool_call.function.name
                ));
            }
        };

        let connection = match self.connections.get(server_name) {
            Some(c) => c,
            None => {
                return Ok(format!(
                    "Error: MCP server '{}' not connected",
                    server_name
                ));
            }
        };

        // Parse arguments
        let args: serde_json::Value =
            serde_json::from_str(&tool_call.function.arguments).unwrap_or(serde_json::json!({}));

        // Call tool
        let result = match connection.call_tool(&tool_call.function.name, args).await {
            Ok(r) => r,
            Err(e) => {
                return Ok(format!("Error calling tool '{}': {}", tool_call.function.name, e));
            }
        };

        // Extract text from content
        let text: Vec<String> = result
            .content
            .iter()
            .filter_map(|c| c.text.clone())
            .collect();

        let content = text.join("\n");

        if result.is_error {
            return Ok(format!("Tool error: {}", content));
        }

        Ok(content)
    }

    /// Get names of all discovered tools.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tool_to_server.keys().map(|s| s.as_str()).collect()
    }
}
