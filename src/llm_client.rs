use crate::config::LlmConfig;
use crate::types::*;
use anyhow::{Context, Result};

pub struct LlmClient {
    client: reqwest::Client,
    api_endpoint: String,
    model: String,
    api_key: String,
    temperature: f32,
}

impl LlmClient {
    pub fn new(config: &LlmConfig, api_key: String) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
            api_endpoint: config.api_endpoint.trim_end_matches('/').to_string(),
            model: config.model.clone(),
            api_key,
            temperature: config.temperature,
        })
    }

    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
    ) -> Result<ChatResponse> {
        let url = format!("{}/v1/chat/completions", self.api_endpoint);

        let mut request = ChatRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            tools: tools.map(|t| t.to_vec()),
            tool_choice: None,
            temperature: Some(self.temperature),
            max_tokens: None,
        };

        if request.tools.is_some() {
            request.tool_choice = Some(serde_json::json!("auto"));
        }

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await
            .with_context(|| "Failed to send request to LLM API")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "LLM API error ({}): {}",
                status,
                body.chars().take(500).collect::<String>()
            );
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .with_context(|| "Failed to parse LLM API response")?;

        Ok(chat_response)
    }
}
