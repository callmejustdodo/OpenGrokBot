//! The endpoints. Each one identifies the caller first (so a stranger gets `403`
//! whatever the body says), applies the policy checks the handler traits
//! promise, and hands the call on.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;

use formation_proto::ask::Question;
use formation_proto::common::{ErrorCode, MemberId, QuestionId, RequestId};
use formation_proto::peer::{
    AgentsResponse, PATH_AGENTS, PATH_ASK, PATH_EVENTS, PATH_QUESTION_ACTION, PATH_REQUEST,
    PATH_REQUEST_CANCEL, PATH_REQUESTS, PATH_TEAM, PATH_TEAM_JOIN, PeerAsk, PeerAskAccepted,
    PeerJoin, PeerJoinAccepted, PeerQuestionAction, PeerRequestCreate, RosterResponse,
};
use formation_proto::request::WorkRequest;
use formation_proto::team::WhoIs;

use super::caller::not_member;
use super::error::PeerError;
use super::{ApiState, events, member_caller};

type Shared = State<Arc<ApiState>>;
type Reply<T> = Result<Json<T>, PeerError>;

pub(crate) fn router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route(PATH_AGENTS, get(agents))
        .route(PATH_ASK, post(ask))
        .route(PATH_QUESTION_ACTION, post(question_action))
        .route(PATH_REQUESTS, post(create_request))
        .route(PATH_REQUEST, get(get_request))
        .route(PATH_REQUEST_CANCEL, post(cancel_request))
        .route(PATH_TEAM, get(roster))
        .route(PATH_TEAM_JOIN, post(join))
        .route(PATH_EVENTS, get(open_events))
        .fallback(unknown)
        .method_not_allowed_fallback(unknown)
        .with_state(state)
}

async fn agents(State(state): Shared, caller: Authenticated) -> Reply<AgentsResponse> {
    let caller = member_caller(caller.member_id, None, None);
    let agents = state.policy.visible_agents(&caller).await?;
    Ok(Json(AgentsResponse { agents }))
}

async fn ask(
    State(state): Shared,
    caller: Authenticated,
    Body(ask): Body<PeerAsk>,
) -> Result<(StatusCode, Json<PeerAskAccepted>), PeerError> {
    let handler = state.ask.clone().ok_or_else(not_served)?;
    if state.team.me()? != Some(ask.target.owner()) {
        return Err(PeerError::new(
            ErrorCode::NotFound,
            "the question is for another member's Mac",
        ));
    }
    let caller = member_caller(caller.member_id, ask.via_agent.clone(), Some(ask.chain));
    state.policy.check_ask(&caller, &ask.target).await?;
    let accepted = handler.ask(caller, ask).await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn question_action(
    State(state): Shared,
    caller: Authenticated,
    Path(id): Path<String>,
    Body(action): Body<PeerQuestionAction>,
) -> Reply<Question> {
    let handler = state.ask.clone().ok_or_else(not_served)?;
    let id: QuestionId = parse_id(&id)?;
    Ok(Json(
        handler
            .question_action(caller.member_id, id, action)
            .await?,
    ))
}

async fn create_request(
    State(state): Shared,
    caller: Authenticated,
    Body(request): Body<PeerRequestCreate>,
) -> Reply<WorkRequest> {
    let handler = state.requests.clone().ok_or_else(not_served)?;
    let caller = member_caller(
        caller.member_id,
        request.via_agent.clone(),
        Some(request.chain),
    );
    state
        .policy
        .check_request(&caller, request.agent_id)
        .await?;
    Ok(Json(handler.create(caller, request).await?))
}

async fn get_request(
    State(state): Shared,
    caller: Authenticated,
    Path(id): Path<String>,
) -> Reply<WorkRequest> {
    let handler = state.requests.clone().ok_or_else(not_served)?;
    let id: RequestId = parse_id(&id)?;
    Ok(Json(handler.get(caller.member_id, id).await?))
}

async fn cancel_request(
    State(state): Shared,
    caller: Authenticated,
    Path(id): Path<String>,
) -> Reply<WorkRequest> {
    let handler = state.requests.clone().ok_or_else(not_served)?;
    let id: RequestId = parse_id(&id)?;
    Ok(Json(handler.cancel(caller.member_id, id).await?))
}

async fn roster(State(state): Shared, _caller: Authenticated) -> Reply<RosterResponse> {
    let version = state.team.team()?.map_or(0, |team| team.roster_version);
    Ok(Json(RosterResponse {
        members: state.team.members()?,
        version,
    }))
}

/// The one call taken from a tailnet node that is not a member yet.
async fn join(
    State(state): Shared,
    Identified(whois): Identified,
    Body(join): Body<PeerJoin>,
) -> Reply<PeerJoinAccepted> {
    let handler = state.join.clone().ok_or_else(not_served)?;
    Ok(Json(handler.join(whois, join).await?))
}

async fn open_events(
    State(state): Shared,
    caller: Authenticated,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| events::serve(socket, state, caller.member_id, caller.whois))
}

/// Unknown paths and methods, for members only.
async fn unknown(_caller: Authenticated) -> PeerError {
    PeerError::new(ErrorCode::NotFound, "no such endpoint")
}

fn not_served() -> PeerError {
    PeerError::new(ErrorCode::NotFound, "this Mac does not serve that call")
}

fn parse_id<T: DeserializeOwned>(raw: &str) -> Result<T, PeerError> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned()))
        .map_err(|_| PeerError::new(ErrorCode::InvalidParams, format!("{raw:?} is not an ID")))
}

/// The caller's Tailscale identity, from the connection's peer address.
pub(crate) struct Identified(pub WhoIs);

impl FromRequestParts<Arc<ApiState>> for Identified {
    type Rejection = PeerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<ApiState>,
    ) -> Result<Self, Self::Rejection> {
        let ConnectInfo(addr) = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .copied()
            .ok_or_else(|| PeerError::new(ErrorCode::Internal, "no peer address"))?;
        let whois =
            state.identity.identify(addr).await.map_err(|e| {
                PeerError(not_member(format!("caller not identified: {}", e.message)))
            })?;
        Ok(Self(whois))
    }
}

/// A caller mapped to a member of this Mac's team, other than this Mac's own.
pub(crate) struct Authenticated {
    pub member_id: MemberId,
    pub whois: WhoIs,
}

impl FromRequestParts<Arc<ApiState>> for Authenticated {
    type Rejection = PeerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<ApiState>,
    ) -> Result<Self, Self::Rejection> {
        let Identified(whois) = Identified::from_request_parts(parts, state).await?;
        let member_id = state.authenticate(&whois).await?;
        Ok(Self { member_id, whois })
    }
}

/// A JSON body; one that does not decode is `invalid_params`.
pub(crate) struct Body<T>(pub T);

impl<S, T> FromRequest<S> for Body<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = PeerError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(PeerError::new(
                ErrorCode::InvalidParams,
                rejection.body_text(),
            )),
        }
    }
}
