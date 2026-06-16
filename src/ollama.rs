//! Local Ollama client: model discovery and streaming chat.
//!
//! The HTTP wiring lives in [`HttpOllama`]; the request building and response
//! parsing are pure functions so they can be tested without a running Ollama.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;

/// A model reported by Ollama's `/api/tags`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaModel {
    /// Model identifier (e.g. `llama3.1:8b`).
    pub name: String,
    /// Model family, if reported.
    pub family: Option<String>,
    /// Parameter size, if reported.
    pub parameter_size: Option<String>,
}

/// One streamed event from a chat completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OllamaEvent {
    /// A text fragment.
    Token(String),
    /// Final token usage for the completion.
    Usage {
        /// Prompt (input) tokens.
        input_tokens: u32,
        /// Completion (output) tokens.
        output_tokens: u32,
    },
}

/// A chat request to run on a local model.
#[derive(Debug, Clone)]
pub struct ChatSpec {
    /// Model to run.
    pub model: String,
    /// Optional system prompt.
    pub system: Option<String>,
    /// Conversation messages as (role, content) pairs.
    pub messages: Vec<(String, String)>,
    /// Sampling temperature, if set.
    pub temperature: Option<f32>,
    /// Maximum tokens to generate, if set.
    pub max_tokens: Option<u32>,
}

/// Errors talking to Ollama.
#[derive(Debug, Clone, thiserror::Error)]
pub enum OllamaError {
    /// A network or transport failure.
    #[error("ollama transport error: {0}")]
    Transport(String),
    /// Ollama returned a non-success status.
    #[error("ollama returned an error: {0}")]
    Provider(String),
}

/// A stream of chat events.
pub type OllamaStream = BoxStream<'static, Result<OllamaEvent, OllamaError>>;

/// A local Ollama runtime.
#[async_trait]
pub trait Ollama: Send + Sync {
    /// List the models the runtime currently offers.
    async fn list_models(&self) -> Result<Vec<OllamaModel>, OllamaError>;

    /// Start a streaming chat completion.
    async fn chat(&self, spec: ChatSpec) -> Result<OllamaStream, OllamaError>;
}

// ---- Wire types (Ollama's JSON shapes) ----

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    details: Option<TagDetails>,
}

#[derive(Debug, Deserialize)]
struct TagDetails {
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    parameter_size: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    message: Option<ChatMessageChunk>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ChatMessageChunk {
    #[serde(default)]
    content: String,
}

/// Parse the `/api/tags` body into models.
fn parse_tags(body: &str) -> Result<Vec<OllamaModel>, OllamaError> {
    let parsed: TagsResponse =
        serde_json::from_str(body).map_err(|error| OllamaError::Provider(error.to_string()))?;
    Ok(parsed
        .models
        .into_iter()
        .map(|model| {
            let details = model.details.unwrap_or(TagDetails {
                family: None,
                parameter_size: None,
            });
            OllamaModel {
                name: model.name,
                family: details.family,
                parameter_size: details.parameter_size,
            }
        })
        .collect())
}

/// Parse one NDJSON chat chunk line into an event, if it carries one. A line may
/// hold a token, a final usage report (on `done`), both, or neither.
fn parse_chat_chunk(line: &str) -> Vec<OllamaEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(chunk) = serde_json::from_str::<ChatChunk>(trimmed) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    if let Some(message) = chunk.message
        && !message.content.is_empty()
    {
        events.push(OllamaEvent::Token(message.content));
    }
    if chunk.done {
        events.push(OllamaEvent::Usage {
            input_tokens: chunk.prompt_eval_count.unwrap_or(0),
            output_tokens: chunk.eval_count.unwrap_or(0),
        });
    }
    events
}

/// Build the `/api/chat` request body for a spec.
fn chat_body(spec: &ChatSpec) -> serde_json::Value {
    let mut messages = Vec::new();
    if let Some(system) = &spec.system {
        messages.push(serde_json::json!({ "role": "system", "content": system }));
    }
    for (role, content) in &spec.messages {
        messages.push(serde_json::json!({ "role": role, "content": content }));
    }
    let mut options = serde_json::Map::new();
    if let Some(temperature) = spec.temperature {
        options.insert("temperature".to_owned(), serde_json::json!(temperature));
    }
    if let Some(max_tokens) = spec.max_tokens {
        options.insert("num_predict".to_owned(), serde_json::json!(max_tokens));
    }
    serde_json::json!({
        "model": spec.model,
        "messages": messages,
        "stream": true,
        "options": options,
    })
}

/// Ollama client over HTTP (reqwest).
pub struct HttpOllama {
    base_url: String,
    client: reqwest::Client,
}

impl HttpOllama {
    /// Create a client for the Ollama runtime at `base_url`.
    #[must_use]
    pub fn new(base_url: String, client: reqwest::Client) -> Self {
        Self { base_url, client }
    }
}

#[async_trait]
impl Ollama for HttpOllama {
    async fn list_models(&self) -> Result<Vec<OllamaModel>, OllamaError> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|error| OllamaError::Transport(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OllamaError::Provider(response.status().to_string()));
        }
        let body = response
            .text()
            .await
            .map_err(|error| OllamaError::Transport(error.to_string()))?;
        parse_tags(&body)
    }

    async fn chat(&self, spec: ChatSpec) -> Result<OllamaStream, OllamaError> {
        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&chat_body(&spec))
            .send()
            .await
            .map_err(|error| OllamaError::Transport(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OllamaError::Provider(response.status().to_string()));
        }

        // Ollama streams newline-delimited JSON; buffer partial lines across
        // chunks and emit an event per complete line.
        let mut bytes = response.bytes_stream();
        let stream = async_stream::stream! {
            let mut buffer = String::new();
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield Err(OllamaError::Transport(error.to_string()));
                        break;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(newline) = buffer.find('\n') {
                    let line: String = buffer.drain(..=newline).collect();
                    for event in parse_chat_chunk(&line) {
                        yield Ok(event);
                    }
                }
            }
            for event in parse_chat_chunk(&buffer) {
                yield Ok(event);
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tags_reads_names_and_details() {
        let body = r#"{"models":[
            {"name":"llama3.1:8b","details":{"family":"llama","parameter_size":"8B"}},
            {"name":"qwen3:14b"}
        ]}"#;

        let models = parse_tags(body).unwrap();

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].name, "llama3.1:8b");
        assert_eq!(models[0].family.as_deref(), Some("llama"));
        assert_eq!(models[0].parameter_size.as_deref(), Some("8B"));
        assert_eq!(models[1].name, "qwen3:14b");
        assert_eq!(models[1].family, None);
    }

    #[test]
    fn parse_tags_handles_an_empty_list() {
        assert_eq!(parse_tags(r#"{"models":[]}"#).unwrap(), Vec::new());
        assert_eq!(parse_tags("{}").unwrap(), Vec::new());
    }

    #[test]
    fn parse_chat_chunk_extracts_tokens() {
        let events = parse_chat_chunk(r#"{"message":{"content":"Hello"},"done":false}"#);
        assert_eq!(events, vec![OllamaEvent::Token("Hello".to_owned())]);
    }

    #[test]
    fn parse_chat_chunk_extracts_usage_on_done() {
        let events = parse_chat_chunk(
            r#"{"message":{"content":""},"done":true,"prompt_eval_count":10,"eval_count":20}"#,
        );
        assert_eq!(
            events,
            vec![OllamaEvent::Usage {
                input_tokens: 10,
                output_tokens: 20
            }]
        );
    }

    #[test]
    fn parse_chat_chunk_skips_blank_and_invalid_lines() {
        assert!(parse_chat_chunk("").is_empty());
        assert!(parse_chat_chunk("   ").is_empty());
        assert!(parse_chat_chunk("not json").is_empty());
    }

    #[test]
    fn chat_body_includes_system_and_options() {
        let spec = ChatSpec {
            model: "llama3.1:8b".to_owned(),
            system: Some("be terse".to_owned()),
            messages: vec![("user".to_owned(), "hi".to_owned())],
            temperature: Some(0.5),
            max_tokens: Some(128),
        };

        let body = chat_body(&spec);

        assert_eq!(body["model"], "llama3.1:8b");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["options"]["temperature"], 0.5);
        assert_eq!(body["options"]["num_predict"], 128);
    }

    #[test]
    fn chat_body_omits_absent_options() {
        let spec = ChatSpec {
            model: "m".to_owned(),
            system: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
        };

        let body = chat_body(&spec);

        assert_eq!(body["options"].as_object().unwrap().len(), 0);
        assert_eq!(body["messages"].as_array().unwrap().len(), 0);
    }
}
