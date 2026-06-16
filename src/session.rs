//! Drives one connected session: announce models, then serve inference frames.
//!
//! The transport is reduced to two channels (incoming server frames, outgoing
//! client frames), so this logic is testable without a real websocket. Each
//! inference runs as its own task and can be cancelled independently.

use std::collections::HashMap;
use std::sync::Arc;

use crate::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame, WireModel};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::ollama::{ChatSpec, Ollama, OllamaEvent};

/// The agent's own version, announced in the handshake.
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run a session until the incoming channel closes or the server rejects it.
///
/// Sends `Hello` and the current model list, then handles `Infer`/`Cancel`/`Ping`
/// frames. Returns an error only if the server rejected the handshake.
///
/// # Errors
/// Returns an error if the server sends a `Reject` frame.
pub async fn run_session(
    ollama: Arc<dyn Ollama>,
    mut incoming: mpsc::UnboundedReceiver<ServerFrame>,
    outgoing: mpsc::UnboundedSender<ClientFrame>,
) -> anyhow::Result<()> {
    handshake(ollama.as_ref(), &outgoing).await;

    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Uuid>();
    let mut tasks: HashMap<Uuid, AbortHandle> = HashMap::new();

    loop {
        tokio::select! {
            frame = incoming.recv() => {
                match frame {
                    Some(frame) => {
                        if !handle_frame(&ollama, &outgoing, &done_tx, &mut tasks, frame)? {
                            break;
                        }
                    }
                    None => break,
                }
            }
            Some(correlation) = done_rx.recv() => {
                tasks.remove(&correlation);
            }
        }
    }

    // Abort any inferences still running when the connection drops.
    for handle in tasks.values() {
        handle.abort();
    }
    Ok(())
}

/// Announce the protocol version and the current model list.
async fn handshake(ollama: &dyn Ollama, outgoing: &mpsc::UnboundedSender<ClientFrame>) {
    let _ = outgoing.send(ClientFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        agent_version: AGENT_VERSION.to_owned(),
    });
    let models = match ollama.list_models().await {
        Ok(models) => models
            .into_iter()
            .map(|model| WireModel {
                name: model.name,
                family: model.family,
                parameter_size: model.parameter_size,
            })
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "failed to list local models; reporting none");
            Vec::new()
        }
    };
    let _ = outgoing.send(ClientFrame::Models { models });
}

/// Handle one server frame. Returns whether the session should continue.
fn handle_frame(
    ollama: &Arc<dyn Ollama>,
    outgoing: &mpsc::UnboundedSender<ClientFrame>,
    done_tx: &mpsc::UnboundedSender<Uuid>,
    tasks: &mut HashMap<Uuid, AbortHandle>,
    frame: ServerFrame,
) -> anyhow::Result<bool> {
    match frame {
        ServerFrame::Welcome { .. } => {}
        ServerFrame::Reject { code, message } => {
            anyhow::bail!("server rejected the connection ({code}): {message}");
        }
        ServerFrame::Ping => {
            let _ = outgoing.send(ClientFrame::Pong);
        }
        ServerFrame::Infer {
            correlation,
            model,
            system,
            messages,
            params,
        } => {
            let spec = ChatSpec {
                model,
                system,
                messages: messages
                    .into_iter()
                    .map(|message| (message.role, message.content))
                    .collect(),
                temperature: params.temperature,
                max_tokens: params.max_tokens,
            };
            let handle = tokio::spawn(run_inference(
                ollama.clone(),
                spec,
                correlation,
                outgoing.clone(),
                done_tx.clone(),
            ));
            tasks.insert(correlation, handle.abort_handle());
        }
        ServerFrame::Cancel { correlation } => {
            if let Some(handle) = tasks.remove(&correlation) {
                handle.abort();
            }
        }
    }
    Ok(true)
}

/// Run one inference to completion, streaming frames back. On normal completion
/// it sends `Done`; on error, `Failed`. Cancellation drops the task before either.
async fn run_inference(
    ollama: Arc<dyn Ollama>,
    spec: ChatSpec,
    correlation: Uuid,
    outgoing: mpsc::UnboundedSender<ClientFrame>,
    done_tx: mpsc::UnboundedSender<Uuid>,
) {
    match ollama.chat(spec).await {
        Err(error) => {
            let _ = outgoing.send(ClientFrame::Failed {
                correlation,
                message: error.to_string(),
            });
        }
        Ok(mut stream) => {
            let mut failed = false;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(OllamaEvent::Token(text)) => {
                        let _ = outgoing.send(ClientFrame::Token { correlation, text });
                    }
                    Ok(OllamaEvent::Usage {
                        input_tokens,
                        output_tokens,
                    }) => {
                        let _ = outgoing.send(ClientFrame::Usage {
                            correlation,
                            input_tokens,
                            output_tokens,
                        });
                    }
                    Err(error) => {
                        let _ = outgoing.send(ClientFrame::Failed {
                            correlation,
                            message: error.to_string(),
                        });
                        failed = true;
                        break;
                    }
                }
            }
            if !failed {
                let _ = outgoing.send(ClientFrame::Done { correlation });
            }
        }
    }
    let _ = done_tx.send(correlation);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::protocol::{WireMessage, WireParams};
    use async_trait::async_trait;

    use super::*;
    use crate::ollama::{OllamaError, OllamaModel, OllamaStream};

    struct FakeOllama {
        models: Vec<OllamaModel>,
        events: Vec<Result<OllamaEvent, OllamaError>>,
    }

    #[async_trait]
    impl Ollama for FakeOllama {
        async fn list_models(&self) -> Result<Vec<OllamaModel>, OllamaError> {
            Ok(self.models.clone())
        }

        async fn chat(&self, _spec: ChatSpec) -> Result<OllamaStream, OllamaError> {
            let events = self.events.clone();
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn infer(correlation: Uuid) -> ServerFrame {
        ServerFrame::Infer {
            correlation,
            model: "m".to_owned(),
            system: None,
            messages: vec![WireMessage {
                role: "user".to_owned(),
                content: "hi".to_owned(),
            }],
            params: WireParams {
                temperature: None,
                max_tokens: None,
            },
        }
    }

    #[tokio::test]
    async fn announces_hello_and_models_on_start() {
        let ollama = Arc::new(FakeOllama {
            models: vec![OllamaModel {
                name: "llama3.1:8b".to_owned(),
                family: None,
                parameter_size: None,
            }],
            events: vec![],
        });
        let (_in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run_session(ollama, in_rx, out_tx));

        assert!(matches!(
            out_rx.recv().await.unwrap(),
            ClientFrame::Hello { protocol_version, .. } if protocol_version == PROTOCOL_VERSION
        ));
        match out_rx.recv().await.unwrap() {
            ClientFrame::Models { models } => {
                assert_eq!(models[0].name, "llama3.1:8b");
            }
            other => panic!("expected models, got {other:?}"),
        }
        handle.abort();
    }

    #[tokio::test]
    async fn an_infer_streams_token_usage_then_done() {
        let ollama = Arc::new(FakeOllama {
            models: vec![],
            events: vec![
                Ok(OllamaEvent::Token("Hi".to_owned())),
                Ok(OllamaEvent::Usage {
                    input_tokens: 3,
                    output_tokens: 1,
                }),
            ],
        });
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_session(ollama, in_rx, out_tx));

        // Drain Hello + Models.
        out_rx.recv().await;
        out_rx.recv().await;

        let correlation = Uuid::from_u128(7);
        in_tx.send(infer(correlation)).unwrap();

        assert!(matches!(
            out_rx.recv().await.unwrap(),
            ClientFrame::Token { text, .. } if text == "Hi"
        ));
        assert!(matches!(
            out_rx.recv().await.unwrap(),
            ClientFrame::Usage {
                input_tokens: 3,
                ..
            }
        ));
        assert!(matches!(
            out_rx.recv().await.unwrap(),
            ClientFrame::Done { .. }
        ));
        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_chat_error_surfaces_as_failed() {
        let ollama = Arc::new(FakeOllama {
            models: vec![],
            events: vec![Err(OllamaError::Provider("boom".to_owned()))],
        });
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_session(ollama, in_rx, out_tx));
        out_rx.recv().await;
        out_rx.recv().await;

        in_tx.send(infer(Uuid::from_u128(1))).unwrap();

        assert!(matches!(
            out_rx.recv().await.unwrap(),
            ClientFrame::Failed { .. }
        ));
        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn ping_is_answered_with_pong() {
        let ollama = Arc::new(FakeOllama {
            models: vec![],
            events: vec![],
        });
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_session(ollama, in_rx, out_tx));
        out_rx.recv().await;
        out_rx.recv().await;

        in_tx.send(ServerFrame::Ping).unwrap();

        assert!(matches!(out_rx.recv().await.unwrap(), ClientFrame::Pong));
        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_reject_frame_ends_the_session_with_an_error() {
        let ollama = Arc::new(FakeOllama {
            models: vec![],
            events: vec![],
        });
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_session(ollama, in_rx, out_tx));
        out_rx.recv().await;
        out_rx.recv().await;

        in_tx
            .send(ServerFrame::Reject {
                code: "protocol_version_mismatch".to_owned(),
                message: "old".to_owned(),
            })
            .unwrap();

        let result = handle.await.unwrap();
        assert!(result.is_err());
    }
}
