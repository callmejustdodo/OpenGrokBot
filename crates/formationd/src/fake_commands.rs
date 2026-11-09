use std::sync::Arc;

use async_trait::async_trait;
use formation_core::{agents::HarnessReadiness, ports::*, testkit::FakeHarness};
use formation_proto::{
    agent::*,
    common::*,
    harness::{AgentMessageSend, AgentTurnInterrupt, ConversationPage},
    policy::Caller,
    request::{QueueItem, QueueSource},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

pub struct FakeCommands {
    pub harness: Arc<FakeHarness>,
    pub store: Arc<dyn Store>,
    pub status: HarnessStatus,
}

impl HarnessReadiness for FakeCommands {
    fn check(&self) -> Result<Vec<HarnessStatus>> {
        Ok(vec![self.status.clone()])
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| ApiError::new(ErrorCode::InvalidParams, e.to_string()))
}
fn encode(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

#[async_trait]
impl CommandHandler for FakeCommands {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            "agent_start",
            "agent_stop",
            "agent_message_send",
            "agent_turn_interrupt",
            "conversation_page",
        ]
    }

    async fn handle(&self, call: CommandCall) -> Result<Value> {
        if call.caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "main sessions belong to the owner",
            ));
        }
        match call.method.as_str() {
            "agent_start" => {
                let p: AgentStart = decode(call.params)?;
                let agent = self
                    .store
                    .agent(p.agent_id)?
                    .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "agent not found"))?;
                self.harness.start(&agent, None).await?;
                encode(agent)
            }
            "agent_stop" => {
                let p: AgentStop = decode(call.params)?;
                self.harness.stop(p.agent_id, p.mode).await?;
                encode(
                    self.store
                        .agent(p.agent_id)?
                        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "agent not found"))?,
                )
            }
            "agent_message_send" => {
                let p: AgentMessageSend = decode(call.params)?;
                if p.text.trim().is_empty() {
                    return Err(ApiError::new(
                        ErrorCode::InvalidParams,
                        "message must not be empty",
                    ));
                }
                let item = QueueItem {
                    id: MessageId::generate(),
                    agent_id: p.agent_id,
                    source: QueueSource::Owner,
                    text: p.text,
                    queued_at: std::time::SystemTime::now().into(),
                };
                self.harness
                    .run_turn(p.agent_id, RunId::generate(), item.clone())
                    .await?;
                encode(item)
            }
            "agent_turn_interrupt" => {
                let p: AgentTurnInterrupt = decode(call.params)?;
                self.harness.interrupt(p.agent_id).await?;
                encode(())
            }
            "conversation_page" => {
                let p: ConversationPage = decode(call.params)?;
                encode(
                    self.harness
                        .conversation(p.agent_id, p.session_id, p.before, p.limit)
                        .await?,
                )
            }
            _ => Err(ApiError::new(
                ErrorCode::HarnessUnsupported,
                "no scripted fake turn configured",
            )),
        }
    }
}
