//! Wire protocol between the local bridge agent and the backend.
//!
//! This module mirrors the backend's `bridge-protocol` definitions; the two must
//! agree on the frame shapes. Drift is caught at connect time: the agent
//! announces [`PROTOCOL_VERSION`] in its [`ClientFrame::Hello`] and the backend
//! rejects a mismatch. It uses only primitive types so the agent stays small.
//!
//! Frames are JSON, tagged by a `type` field.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The wire protocol version. Bumped on any breaking frame change.
pub const PROTOCOL_VERSION: u16 = 1;

/// One chat message forwarded to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMessage {
    /// Author role: `system`, `user`, or `assistant`.
    pub role: String,
    /// Message text.
    pub content: String,
}

/// Sampling and length parameters for an inference.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WireParams {
    /// Sampling temperature, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum number of tokens to generate, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// A model a connected agent offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireModel {
    /// Model identifier as the local runtime reports it (e.g. `llama3.1:8b`).
    pub name: String,
    /// Model family, if known (e.g. `llama`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Human-readable parameter size, if known (e.g. `8B`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_size: Option<String>,
}

/// A frame the cloud sends down to the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// The handshake was accepted; the session is open.
    Welcome {
        /// The opaque session id (informational; the agent need not act on it).
        session_id: Uuid,
    },
    /// The handshake was rejected (e.g. protocol mismatch); the socket closes.
    Reject {
        /// Machine-readable reason code.
        code: String,
        /// Human-readable detail.
        message: String,
    },
    /// Run an inference on the local model.
    Infer {
        /// Correlates the agent's reply frames to this request.
        correlation: Uuid,
        /// The model to run.
        model: String,
        /// Optional system prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<String>,
        /// Conversation messages, oldest first.
        messages: Vec<WireMessage>,
        /// Sampling/length parameters.
        params: WireParams,
    },
    /// Stop an in-flight inference.
    Cancel {
        /// The inference to cancel.
        correlation: Uuid,
    },
    /// Liveness probe; the agent replies with [`ClientFrame::Pong`].
    Ping,
}

/// A frame the agent sends up to the cloud.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// First frame after connecting: announce protocol and agent version.
    Hello {
        /// The protocol version the agent speaks.
        protocol_version: u16,
        /// The agent's own version string (informational).
        agent_version: String,
    },
    /// Report (or update) the models the agent currently offers.
    Models {
        /// The offered models.
        models: Vec<WireModel>,
    },
    /// A text fragment of a completion.
    Token {
        /// The inference this fragment belongs to.
        correlation: Uuid,
        /// The text fragment.
        text: String,
    },
    /// The final token usage for a completion.
    Usage {
        /// The inference this usage belongs to.
        correlation: Uuid,
        /// Prompt (input) token count.
        input_tokens: u32,
        /// Completion (output) token count.
        output_tokens: u32,
    },
    /// A completion finished successfully.
    Done {
        /// The inference that finished.
        correlation: Uuid,
    },
    /// A completion failed on the agent's side.
    Failed {
        /// The inference that failed.
        correlation: Uuid,
        /// A human-readable reason.
        message: String,
    },
    /// Reply to a [`ServerFrame::Ping`].
    Pong,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_infer_frame_round_trips_as_tagged_json() {
        let frame = ServerFrame::Infer {
            correlation: Uuid::from_u128(1),
            model: "llama3.1:8b".to_owned(),
            system: Some("be terse".to_owned()),
            messages: vec![WireMessage {
                role: "user".to_owned(),
                content: "hi".to_owned(),
            }],
            params: WireParams {
                temperature: Some(0.7),
                max_tokens: Some(256),
            },
        };

        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"type\":\"infer\""));
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn client_hello_and_usage_round_trip() {
        let hello = ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
        };
        let usage = ClientFrame::Usage {
            correlation: Uuid::from_u128(2),
            input_tokens: 10,
            output_tokens: 20,
        };

        for frame in [hello, usage] {
            let json = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
        }
    }

    #[test]
    fn optional_params_are_omitted_when_absent() {
        let frame = ServerFrame::Infer {
            correlation: Uuid::from_u128(3),
            model: "m".to_owned(),
            system: None,
            messages: vec![],
            params: WireParams {
                temperature: None,
                max_tokens: None,
            },
        };

        let json = serde_json::to_string(&frame).unwrap();
        assert!(!json.contains("system"));
        assert!(!json.contains("temperature"));
    }
}
