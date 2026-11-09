//! `WS /v1/events`: presence, question, request, and roster updates for one
//! caller.
//!
//! Each connection starts with this Mac's [`hello`] (online, with the caller's
//! view of its shared agents), then re-sends the caller's questions and requests
//! the handlers name (so nothing published while it was closed is lost), then
//! forwards what the modules publish on the [`PeerEventHub`] for
//! that caller. The server pings every heartbeat so the other side notices a
//! dead link. A connection whose member was removed is closed (backend
//! architecture, "Where each rule is enforced").

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::{Instant, interval_at};

use formation_proto::common::MemberId;
use formation_proto::peer::PeerEvent;
use formation_proto::team::WhoIs;

use super::ApiState;
use crate::peer::outbox::{now, stopped};
use crate::peer::presence::hello;

/// Who an event is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// One member's connections: question and request updates go to the asker.
    Member(MemberId),
    /// Every member: roster changes from the creator's Mac.
    Members,
}

impl Audience {
    fn includes(self, member: MemberId) -> bool {
        match self {
            Self::Member(target) => target == member,
            Self::Members => true,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum HubMessage {
    Event {
        audience: Audience,
        event: PeerEvent,
    },
    AgentsChanged,
    Recheck,
}

/// Where the `team`, `ask`, and `requests` modules publish what callers may see.
/// Cheap to clone. A connection that falls too far behind is closed; its Mac
/// reconnects and gets the open items again.
#[derive(Debug, Clone)]
pub struct PeerEventHub {
    tx: broadcast::Sender<HubMessage>,
}

impl Default for PeerEventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerEventHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx }
    }

    /// To one member's connections.
    pub fn publish_to(&self, member: MemberId, event: PeerEvent) {
        self.send(HubMessage::Event {
            audience: Audience::Member(member),
            event,
        });
    }

    /// To every member. A [`PeerEvent::Roster`] also makes every connection check
    /// its member again first, so a removed member's connection closes instead.
    pub fn publish_all(&self, event: PeerEvent) {
        self.send(HubMessage::Event {
            audience: Audience::Members,
            event,
        });
    }

    /// This Mac's shared agents changed: every connection re-sends its hello.
    pub fn agents_changed(&self) {
        self.send(HubMessage::AgentsChanged);
    }

    /// Membership may have changed: every connection checks its member again and
    /// closes if they are no longer one.
    pub fn recheck_members(&self) {
        self.send(HubMessage::Recheck);
    }

    /// Open events connections.
    pub fn connections(&self) -> usize {
        self.tx.receiver_count()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<HubMessage> {
        self.tx.subscribe()
    }

    fn send(&self, message: HubMessage) {
        // No receiver just means no teammate is connected.
        let _ = self.tx.send(message);
    }
}

pub(crate) async fn serve(
    mut socket: WebSocket,
    state: Arc<ApiState>,
    member: MemberId,
    whois: WhoIs,
) {
    // Subscribe first, so nothing published while the hello is sent is lost.
    let mut hub = state.hub.subscribe();
    let mut shutdown = state.shutdown.clone();
    if send_hello(&mut socket, &state, member).await.is_err()
        || send_catch_up(&mut socket, &state, member).await.is_err()
    {
        return;
    }
    let mut heartbeat = interval_at(Instant::now() + state.heartbeat, state.heartbeat);
    loop {
        tokio::select! {
            message = hub.recv() => {
                let keep = match message {
                    Ok(HubMessage::Event { audience, event }) => {
                        if !audience.includes(member) {
                            continue;
                        }
                        if matches!(event, PeerEvent::Roster { .. })
                            && !still_member(&state, &whois, member).await
                        {
                            close(&mut socket, "no longer a member").await;
                            return;
                        }
                        send(&mut socket, &event).await.is_ok()
                    }
                    Ok(HubMessage::AgentsChanged) => {
                        send_hello(&mut socket, &state, member).await.is_ok()
                    }
                    Ok(HubMessage::Recheck) => {
                        if !still_member(&state, &whois, member).await {
                            close(&mut socket, "no longer a member").await;
                            return;
                        }
                        true
                    }
                    Err(RecvError::Lagged(_)) => {
                        close(&mut socket, "fell behind; reconnect").await;
                        return;
                    }
                    Err(RecvError::Closed) => false,
                };
                if !keep {
                    return;
                }
            }
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                // Pongs; the other side sends nothing else.
                Some(Ok(_)) => {}
            },
            _ = heartbeat.tick() => {
                if socket.send(Message::Ping(Bytes::new())).await.is_err() {
                    return;
                }
            }
            _ = stopped(&mut shutdown) => {
                close(&mut socket, "server stopping").await;
                return;
            }
        }
    }
}

async fn send_hello(socket: &mut WebSocket, state: &ApiState, member: MemberId) -> Result<(), ()> {
    let me = state.team.me().ok().flatten().ok_or(())?;
    let agents = state
        .policy
        .visible_agents(&super::member_caller(member, None, None))
        .await
        .map_err(|_| ())?;
    let presence = hello(me, agents, now());
    send(socket, &PeerEvent::Presence { presence }).await
}

async fn send_catch_up(
    socket: &mut WebSocket,
    state: &ApiState,
    member: MemberId,
) -> Result<(), ()> {
    if let Some(ask) = &state.ask {
        for question in ask.questions_to_resend(member).await.unwrap_or_default() {
            let event = PeerEvent::QuestionUpdate {
                question: Box::new(question),
            };
            send(socket, &event).await?;
        }
    }
    if let Some(requests) = &state.requests {
        for request in requests
            .requests_to_resend(member)
            .await
            .unwrap_or_default()
        {
            let event = PeerEvent::RequestUpdate {
                request: Box::new(request),
            };
            send(socket, &event).await?;
        }
    }
    Ok(())
}

async fn still_member(state: &ApiState, whois: &WhoIs, member: MemberId) -> bool {
    state.authenticate(whois).await.ok() == Some(member)
}

async fn send(socket: &mut WebSocket, event: &PeerEvent) -> Result<(), ()> {
    let json = serde_json::to_string(event).map_err(|_| ())?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

async fn close(socket: &mut WebSocket, reason: &'static str) {
    let frame = CloseFrame {
        code: close_code::POLICY,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}
