use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ai::types::{AiConfig, Provider, TokenUsage};
use crate::error::{IaError, Result};

/// Client for LLM chat APIs — supports both OpenAI-compatible and Anthropic
/// Messages API wire formats.
pub struct LlmClient {
    http: reqwest::Client,
    config: AiConfig,
}

/// Content part for vision-capable messages (public for QA pipeline).
///
/// Constructed using helper methods. Serialized differently depending on the
/// target provider — callers build `MessageContent` values once and the client
/// converts them to the provider-specific JSON when sending.
#[derive(Debug, Clone)]
pub enum MessageContent {
    /// Plain text content.
    Text { text: String },
    /// Base64-encoded image.
    ImageBase64 { media_type: String, data: String },
    /// Image referenced by URL.
    ImageUrl { url: String },
}

impl MessageContent {
    /// Create a text content part.
    pub fn text(s: impl Into<String>) -> Self {
        MessageContent::Text { text: s.into() }
    }

    /// Create a base64 image content part from raw image data.
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        MessageContent::ImageBase64 {
            media_type: media_type.into(),
            data: data.into(),
        }
    }

    /// Create an image content part from a plain URL (not base64).
    pub fn image_url(url: impl Into<String>) -> Self {
        MessageContent::ImageUrl { url: url.into() }
    }

    /// Serialize to OpenAI vision format.
    pub fn to_openai_json(&self) -> serde_json::Value {
        match self {
            Self::Text { text } => serde_json::json!({
                "type": "text",
                "text": text,
            }),
            Self::ImageBase64 { media_type, data } => {
                let data_uri = format!("data:{media_type};base64,{data}");
                serde_json::json!({
                    "type": "image_url",
                    "image_url": { "url": data_uri },
                })
            }
            Self::ImageUrl { url } => serde_json::json!({
                "type": "image_url",
                "image_url": { "url": url },
            }),
        }
    }

    /// Serialize to Anthropic Messages API format.
    pub fn to_anthropic_json(&self) -> serde_json::Value {
        match self {
            Self::Text { text } => serde_json::json!({
                "type": "text",
                "text": text,
            }),
            Self::ImageBase64 { media_type, data } => serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data,
                },
            }),
            Self::ImageUrl { url } => serde_json::json!({
                "type": "image",
                "source": {
                    "type": "url",
                    "url": url,
                },
            }),
        }
    }
}

// ── OpenAI wire format structs ──────────────────────────────────────────

/// A chat message in the OpenAI format (text-only).
#[derive(Debug, Clone, Serialize)]
struct OaiChatMessage {
    role: String,
    content: String,
}

/// A chat message with multi-part content (text + images).
#[derive(Debug, Clone, Serialize)]
struct OaiVisionMessage {
    role: String,
    content: serde_json::Value,
}

/// Request body for the OpenAI chat completions endpoint (text-only).
#[derive(Debug, Serialize)]
struct OaiChatRequest {
    model: String,
    messages: Vec<OaiChatMessage>,
    temperature: f64,
    max_tokens: u64,
}

/// Request body for the OpenAI chat completions endpoint (vision).
#[derive(Debug, Serialize)]
struct OaiVisionRequest {
    model: String,
    messages: Vec<OaiVisionMessage>,
    temperature: f64,
    max_tokens: u64,
}

/// Response from the OpenAI chat completions endpoint.
#[derive(Debug, Deserialize)]
struct OaiResponse {
    choices: Vec<OaiChoice>,
    usage: Option<OaiUsage>,
}

#[derive(Debug, Deserialize)]
struct OaiChoice {
    message: OaiChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct OaiChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OaiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

// ── Anthropic wire format structs ───────────────────────────────────────

/// Request body for the Anthropic Messages API.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    temperature: f64,
    max_tokens: u64,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: serde_json::Value,
}

/// Response from the Anthropic Messages API.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ── LlmResponse ─────────────────────────────────────────────────────────

/// The parsed result of an LLM call.
#[derive(Debug)]
pub struct LlmResponse {
    /// The text content of the response.
    pub content: String,
    /// Token usage if reported by the API.
    pub token_usage: Option<TokenUsage>,
}

// ── LlmClient ───────────────────────────────────────────────────────────

impl LlmClient {
    /// Create a new LLM client with the given configuration.
    pub fn new(config: AiConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| IaError::Config(format!("failed to build HTTP client: {}", e)))?;
        Ok(Self { http, config })
    }

    /// Send a text-only chat request and return the parsed response.
    ///
    /// Dispatches to the appropriate provider format (OpenAI or Anthropic).
    /// Retries with exponential backoff on 429/5xx errors.
    pub async fn chat(&self, system_prompt: &str, user_message: &str) -> Result<LlmResponse> {
        let url = self.config.provider.endpoint_url(&self.config.base_url);

        match self.config.provider {
            Provider::OpenAi => {
                let request = OaiChatRequest {
                    model: self.config.model.clone(),
                    messages: vec![
                        OaiChatMessage {
                            role: "system".to_string(),
                            content: system_prompt.to_string(),
                        },
                        OaiChatMessage {
                            role: "user".to_string(),
                            content: user_message.to_string(),
                        },
                    ],
                    temperature: self.config.temperature,
                    max_tokens: self.config.max_tokens,
                };
                self.send_request(&url, &request).await
            }
            Provider::Anthropic => {
                let request = AnthropicRequest {
                    model: self.config.model.clone(),
                    system: Some(system_prompt.to_string()),
                    messages: vec![AnthropicMessage {
                        role: "user".to_string(),
                        content: serde_json::Value::String(user_message.to_string()),
                    }],
                    temperature: self.config.temperature,
                    max_tokens: self.config.max_tokens,
                };
                self.send_request(&url, &request).await
            }
        }
    }

    /// Send a vision-capable chat request with multi-part content.
    ///
    /// Dispatches to the appropriate provider format (OpenAI or Anthropic).
    /// Retries with exponential backoff on 429/5xx errors.
    pub async fn chat_vision(
        &self,
        system_prompt: &str,
        user_content: Vec<MessageContent>,
    ) -> Result<LlmResponse> {
        let url = self.config.provider.endpoint_url(&self.config.base_url);

        match self.config.provider {
            Provider::OpenAi => {
                let content_json: Vec<serde_json::Value> =
                    user_content.iter().map(|c| c.to_openai_json()).collect();

                let request = OaiVisionRequest {
                    model: self.config.model.clone(),
                    messages: vec![
                        OaiVisionMessage {
                            role: "system".to_string(),
                            content: serde_json::Value::String(system_prompt.to_string()),
                        },
                        OaiVisionMessage {
                            role: "user".to_string(),
                            content: serde_json::Value::Array(content_json),
                        },
                    ],
                    temperature: self.config.temperature,
                    max_tokens: self.config.max_tokens,
                };
                self.send_request(&url, &request).await
            }
            Provider::Anthropic => {
                let content_json: Vec<serde_json::Value> =
                    user_content.iter().map(|c| c.to_anthropic_json()).collect();

                let request = AnthropicRequest {
                    model: self.config.model.clone(),
                    system: Some(system_prompt.to_string()),
                    messages: vec![AnthropicMessage {
                        role: "user".to_string(),
                        content: serde_json::Value::Array(content_json),
                    }],
                    temperature: self.config.temperature,
                    max_tokens: self.config.max_tokens,
                };
                self.send_request(&url, &request).await
            }
        }
    }

    /// Shared retry loop for sending LLM requests.
    ///
    /// Adds provider-appropriate auth headers, retries on transient errors,
    /// and dispatches response parsing to the correct provider format.
    async fn send_request<T: Serialize>(&self, url: &str, request: &T) -> Result<LlmResponse> {
        let mut last_error = None;
        let delays = [1, 2, 4, 8, 16];

        for (attempt, delay) in std::iter::once(&0).chain(delays.iter()).enumerate() {
            if attempt > 0 {
                debug!(attempt, delay_secs = delay, "retrying LLM request");
                tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;
            }

            let mut req = self
                .http
                .post(url)
                .header("Content-Type", "application/json");

            // Provider-specific auth headers
            if let Some(ref key) = self.config.api_key {
                match self.config.provider {
                    Provider::OpenAi => {
                        req = req.header("Authorization", format!("Bearer {}", key));
                    }
                    Provider::Anthropic => {
                        req = req
                            .header("x-api-key", key.as_str())
                            .header("anthropic-version", ANTHROPIC_VERSION);
                    }
                }
            }

            let response = match req.json(request).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(attempt, error = %e, "LLM request failed");
                    last_error = Some(IaError::LlmApi {
                        status: 0,
                        message: e.to_string(),
                    });
                    continue;
                }
            };

            let status = response.status().as_u16();

            if status == 429 || status >= 500 {
                let body = response.text().await.unwrap_or_default();
                warn!(attempt, status, body = %body, "LLM API retryable error");
                last_error = Some(IaError::LlmApi {
                    status,
                    message: body,
                });
                continue;
            }

            if !response.status().is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(IaError::LlmApi {
                    status,
                    message: body,
                });
            }

            let body = response.text().await.map_err(|e| IaError::LlmApi {
                status: 0,
                message: format!("failed to read response body: {}", e),
            })?;

            return match self.config.provider {
                Provider::OpenAi => Self::parse_openai_response(&body),
                Provider::Anthropic => Self::parse_anthropic_response(&body),
            };
        }

        Err(last_error.unwrap_or_else(|| IaError::LlmApi {
            status: 0,
            message: "LLM request failed after all retries".to_string(),
        }))
    }

    /// Parse an OpenAI chat completions response.
    fn parse_openai_response(body: &str) -> Result<LlmResponse> {
        let resp: OaiResponse = serde_json::from_str(body).map_err(|e| {
            let preview: String = body.chars().take(500).collect();
            IaError::LlmApi {
                status: 200,
                message: format!("failed to parse LLM response: {} — body: {}", e, preview),
            }
        })?;

        let content = resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| IaError::LlmApi {
                status: 200,
                message: "LLM response had no choices or empty content".to_string(),
            })?;

        let token_usage = resp.usage.map(|u| TokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            cost_estimate_usd: None,
        });

        Ok(LlmResponse {
            content,
            token_usage,
        })
    }

    /// Parse an Anthropic Messages API response.
    fn parse_anthropic_response(body: &str) -> Result<LlmResponse> {
        let resp: AnthropicResponse = serde_json::from_str(body).map_err(|e| {
            let preview: String = body.chars().take(500).collect();
            IaError::LlmApi {
                status: 200,
                message: format!(
                    "failed to parse Anthropic response: {} — body: {}",
                    e, preview
                ),
            }
        })?;

        let content = resp
            .content
            .into_iter()
            .find(|b| b.content_type == "text")
            .and_then(|b| b.text)
            .ok_or_else(|| IaError::LlmApi {
                status: 200,
                message: "Anthropic response had no text content block".to_string(),
            })?;

        let token_usage = resp.usage.map(|u| TokenUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            cost_estimate_usd: None,
        });

        Ok(LlmResponse {
            content,
            token_usage,
        })
    }

    /// Access the underlying config.
    pub fn config(&self) -> &AiConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::types::Provider;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(base_url: &str) -> AiConfig {
        AiConfig {
            base_url: base_url.to_string(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
            provider: Provider::OpenAi,
        }
    }

    fn test_config_anthropic(base_url: &str) -> AiConfig {
        AiConfig {
            base_url: base_url.to_string(),
            api_key: Some("sk-ant-test".to_string()),
            model: "claude-sonnet-4-6".to_string(),
            temperature: 0.2,
            max_tokens: 100,
            provider: Provider::Anthropic,
        }
    }

    fn oai_chat_response_body(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content,
                },
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
            }
        })
    }

    fn anthropic_response_body(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": content,
            }],
            "model": "claude-sonnet-4-6",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 75,
            }
        })
    }

    // ── OpenAI provider tests ───────────────────────────────────────────

    #[tokio::test]
    async fn openai_successful_chat_request() {
        let server = MockServer::start().await;
        let body = oai_chat_response_body(r#"[{"field":"title","new_value":"Test"}]"#);

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("Authorization", "Bearer test-key"))
            .and(header("Content-Type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config(&server.uri())).unwrap();
        let response = client.chat("system prompt", "user message").await.unwrap();

        assert!(response.content.contains("title"));
        let usage = response.token_usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
    }

    #[tokio::test]
    async fn openai_no_api_key_omits_auth_header() {
        let server = MockServer::start().await;
        let body = oai_chat_response_body("ok");

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = test_config(&server.uri());
        config.api_key = None;
        let client = LlmClient::new(config).unwrap();
        let response = client.chat("system", "user").await.unwrap();
        assert_eq!(response.content, "ok");
    }

    #[tokio::test]
    async fn openai_permanent_error_no_retry() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .expect(1)
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config(&server.uri())).unwrap();
        let err = client.chat("system", "user").await.unwrap_err();
        match err {
            IaError::LlmApi { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "unauthorized");
            }
            _ => panic!("expected LlmApi error"),
        }
    }

    #[tokio::test]
    async fn openai_response_without_usage() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": { "content": "response" },
            }]
        });

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config(&server.uri())).unwrap();
        let response = client.chat("system", "user").await.unwrap();
        assert_eq!(response.content, "response");
        assert!(response.token_usage.is_none());
    }

    #[tokio::test]
    async fn openai_malformed_response_returns_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config(&server.uri())).unwrap();
        let err = client.chat("system", "user").await.unwrap_err();
        match err {
            IaError::LlmApi { message, .. } => {
                assert!(message.contains("failed to parse"));
            }
            _ => panic!("expected LlmApi error"),
        }
    }

    #[tokio::test]
    async fn openai_empty_choices_returns_error() {
        let server = MockServer::start().await;
        let body = serde_json::json!({ "choices": [] });

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config(&server.uri())).unwrap();
        let err = client.chat("system", "user").await.unwrap_err();
        match err {
            IaError::LlmApi { message, .. } => {
                assert!(message.contains("no choices"));
            }
            _ => panic!("expected LlmApi error"),
        }
    }

    #[tokio::test]
    async fn openai_trailing_slash_in_base_url_handled() {
        let server = MockServer::start().await;
        let body = oai_chat_response_body("ok");

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let mut config = test_config(&format!("{}/", server.uri()));
        config.api_key = None;
        let client = LlmClient::new(config).unwrap();
        let response = client.chat("system", "user").await.unwrap();
        assert_eq!(response.content, "ok");
    }

    // ── Anthropic provider tests ────────────────────────────────────────

    #[tokio::test]
    async fn anthropic_successful_chat_request() {
        let server = MockServer::start().await;
        let body = anthropic_response_body("verified ok");

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .and(header("Content-Type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config_anthropic(&server.uri())).unwrap();
        let response = client.chat("system prompt", "user message").await.unwrap();

        assert_eq!(response.content, "verified ok");
        let usage = response.token_usage.unwrap();
        assert_eq!(usage.prompt_tokens, 200);
        assert_eq!(usage.completion_tokens, 75);
    }

    #[tokio::test]
    async fn anthropic_chat_request_body_format() {
        let server = MockServer::start().await;
        let body = anthropic_response_body("ok");

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "model": "claude-sonnet-4-6",
                "system": "you are helpful",
                "messages": [{"role": "user", "content": "hello"}],
                "temperature": 0.2,
                "max_tokens": 100,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config_anthropic(&server.uri())).unwrap();
        let _ = client.chat("you are helpful", "hello").await.unwrap();
    }

    #[tokio::test]
    async fn anthropic_permanent_error_no_retry() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .expect(1)
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config_anthropic(&server.uri())).unwrap();
        let err = client.chat("system", "user").await.unwrap_err();
        match err {
            IaError::LlmApi { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "invalid api key");
            }
            _ => panic!("expected LlmApi error"),
        }
    }

    #[tokio::test]
    async fn anthropic_response_without_usage() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "content": [{"type": "text", "text": "response"}],
        });

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config_anthropic(&server.uri())).unwrap();
        let response = client.chat("system", "user").await.unwrap();
        assert_eq!(response.content, "response");
        assert!(response.token_usage.is_none());
    }

    #[tokio::test]
    async fn anthropic_empty_content_returns_error() {
        let server = MockServer::start().await;
        let body = serde_json::json!({ "content": [] });

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config_anthropic(&server.uri())).unwrap();
        let err = client.chat("system", "user").await.unwrap_err();
        match err {
            IaError::LlmApi { message, .. } => {
                assert!(message.contains("no text content"));
            }
            _ => panic!("expected LlmApi error"),
        }
    }

    // ── MessageContent format tests ─────────────────────────────────────

    #[test]
    fn message_content_openai_image_url() {
        let content = MessageContent::image_url("https://example.com/image.jpg");
        let json = content.to_openai_json();
        assert_eq!(json["type"], "image_url");
        assert_eq!(json["image_url"]["url"], "https://example.com/image.jpg");
    }

    #[test]
    fn message_content_openai_image_base64() {
        let content = MessageContent::image_base64("image/jpeg", "abc123");
        let json = content.to_openai_json();
        assert_eq!(json["type"], "image_url");
        assert!(json["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn message_content_anthropic_image_url() {
        let content = MessageContent::image_url("https://example.com/image.jpg");
        let json = content.to_anthropic_json();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["type"], "url");
        assert_eq!(json["source"]["url"], "https://example.com/image.jpg");
    }

    #[test]
    fn message_content_anthropic_image_base64() {
        let content = MessageContent::image_base64("image/jpeg", "abc123");
        let json = content.to_anthropic_json();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["type"], "base64");
        assert_eq!(json["source"]["media_type"], "image/jpeg");
        assert_eq!(json["source"]["data"], "abc123");
    }

    #[test]
    fn message_content_text_both_formats() {
        let content = MessageContent::text("hello");
        let oai = content.to_openai_json();
        let ant = content.to_anthropic_json();
        // Both use the same text format
        assert_eq!(oai["type"], "text");
        assert_eq!(oai["text"], "hello");
        assert_eq!(ant["type"], "text");
        assert_eq!(ant["text"], "hello");
    }
}
