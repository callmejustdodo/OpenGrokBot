//! Errors on the wire: `{ "error": { "code", "message" } }` with an HTTP status
//! chosen by code. Clients act on the code; the status is for logs and proxies.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::peer::PeerErrorBody;

/// The HTTP status for each error code. The tech spec fixes only `403` for
/// callers who are not members; the rest follow HTTP's usual meanings.
pub fn status_for(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::NotMember
        | ErrorCode::AgentNotShared
        | ErrorCode::RequestsNotAccepted
        | ErrorCode::Denied => StatusCode::FORBIDDEN,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::InvalidParams => StatusCode::BAD_REQUEST,
        ErrorCode::Conflict => StatusCode::CONFLICT,
        ErrorCode::ApprovalExpired => StatusCode::GONE,
        ErrorCode::HarnessUnsupported => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::PeerOffline | ErrorCode::HarnessUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// An [`ApiError`] as a peer response.
#[derive(Debug)]
pub struct PeerError(pub ApiError);

impl From<ApiError> for PeerError {
    fn from(error: ApiError) -> Self {
        Self(error)
    }
}

impl PeerError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(ApiError::new(code, message))
    }
}

impl IntoResponse for PeerError {
    fn into_response(self) -> Response {
        (
            status_for(self.0.code),
            Json(PeerErrorBody { error: self.0 }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_status_and_forbidden_codes_are_403() {
        for code in ErrorCode::ALL {
            let status = status_for(code);
            assert!(
                status.is_client_error() || status.is_server_error(),
                "{code:?}"
            );
        }
        assert_eq!(status_for(ErrorCode::NotMember), StatusCode::FORBIDDEN);
        assert_eq!(status_for(ErrorCode::AgentNotShared), StatusCode::FORBIDDEN);
        assert_eq!(
            status_for(ErrorCode::RateLimited),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
}
