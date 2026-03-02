use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ai::types::{AiConfig, TokenUsage};
use crate::error::{IaError, Result};

/// Client for OpenAI-compatible chat completions API.
pub struct LlmClient {
    http: reqwest::Client,
    config: AiConfig,
}

/// A chat message in the OpenAI format.
#[derive(Debug, Clone, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// Request body for the chat completions endpoint.
#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f64,
    max_tokens: u64,
}

/// Response from the chat completions endpoint.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

/// The parsed result of an LLM call.
#[derive(Debug)]
pub struct LlmResponse {
    /// The text content of the response.
    pub content: String,
    /// Token usage if reported by the API.
    pub token_usage: Option<TokenUsage>,
}

impl LlmClient {
    /// Create a new LLM client with the given configuration.
    pub fn new(config: AiConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| IaError::Config(format!("failed to build HTTP client: {}", e)))?;
        Ok(Self { http, config })
    }

    /// Send a chat completion request and return the parsed response.
    ///
    /// Retries with exponential backoff on 429/5xx errors.
    pub async fn chat(
        &self,
        system_prompt: &str,
        user_message: &str,
    ) -> Result<LlmResponse> {
        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_message.to_string(),
                },
            ],
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
        };

        let mut last_error = None;
        let delays = [1, 2, 4, 8, 16];

        for (attempt, delay) in std::iter::once(&0).chain(delays.iter()).enumerate() {
            if attempt > 0 {
                debug!(attempt, delay_secs = delay, "retrying LLM request");
                tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;
            }

            let mut req = self
                .http
                .post(&url)
                .header("Content-Type", "application/json");

            if let Some(ref key) = self.config.api_key {
                req = req.header("Authorization", format!("Bearer {}", key));
            }

            let response = match req.json(&request).send().await {
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

            let chat_response: ChatResponse =
                serde_json::from_str(&body).map_err(|e| {
                    let preview: String = body.chars().take(500).collect();
                    IaError::LlmApi {
                        status: 200,
                        message: format!("failed to parse LLM response: {} — body: {}", e, preview),
                    }
                })?;

            let content = chat_response
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content)
                .ok_or_else(|| IaError::LlmApi {
                    status: 200,
                    message: "LLM response had no choices or empty content".to_string(),
                })?;

            let token_usage = chat_response.usage.map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                cost_estimate_usd: None,
            });

            return Ok(LlmResponse {
                content,
                token_usage,
            });
        }

        Err(last_error.unwrap_or_else(|| IaError::LlmApi {
            status: 0,
            message: "LLM request failed after all retries".to_string(),
        }))
    }

    /// Access the underlying config.
    pub fn config(&self) -> &AiConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(base_url: &str) -> AiConfig {
        AiConfig {
            base_url: base_url.to_string(),
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 100,
        }
    }

    fn chat_response_body(content: &str) -> serde_json::Value {
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

    #[tokio::test]
    async fn successful_chat_request() {
        let server = MockServer::start().await;
        let body = chat_response_body(r#"[{"field":"title","new_value":"Test"}]"#);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
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
    async fn no_api_key_omits_auth_header() {
        let server = MockServer::start().await;
        let body = chat_response_body("ok");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
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
    async fn permanent_error_no_retry() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .expect(1) // Should only be called once (no retry)
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
    async fn response_without_usage() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": { "content": "response" },
            }]
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let client = LlmClient::new(test_config(&server.uri())).unwrap();
        let response = client.chat("system", "user").await.unwrap();
        assert_eq!(response.content, "response");
        assert!(response.token_usage.is_none());
    }

    #[tokio::test]
    async fn malformed_response_returns_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
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
    async fn empty_choices_returns_error() {
        let server = MockServer::start().await;
        let body = serde_json::json!({ "choices": [] });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
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
    async fn trailing_slash_in_base_url_handled() {
        let server = MockServer::start().await;
        let body = chat_response_body("ok");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let mut config = test_config(&format!("{}/", server.uri()));
        config.api_key = None;
        let client = LlmClient::new(config).unwrap();
        let response = client.chat("system", "user").await.unwrap();
        assert_eq!(response.content, "ok");
    }
}
