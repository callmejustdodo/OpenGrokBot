//! The work request status machine over [`RequestStatus`] (FR-014).
//!
//! | From | To | Caused by |
//! | --- | --- | --- |
//! | `received` | `queued` | [`Requests`](super::Requests) right after it stores the request |
//! | `received`, `queued` | `canceled` | the requester (or the owner) before the run starts |
//! | `queued` | `running` | the executor taking the item and reporting `Started` |
//! | `running` | `awaiting_approval` | the approvals module when a tool call needs the owner |
//! | `awaiting_approval` | `running` | the owner allowing the call |
//! | `awaiting_approval` | `denied`, `expired` | the owner denying it, or 24 hours without an answer (FR-015) |
//! | `running` | `completed` | the executor reporting `Completed` |
//! | any non-final | `failed` | a run failure or interruption, or a restart that cannot tell whether the run started |
//!
//! Approvals are raised per tool call while the request runs (FR-015, and the
//! request runs in a fork whose tools go through the permission gate, #14), so
//! `awaiting_approval` is entered from `running` only, and a request that waits for
//! approval has already started and can no longer be canceled. Final statuses
//! ([`RequestStatus::is_final`]) never change.

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::request::RequestStatus::{
    self, AwaitingApproval, Canceled, Completed, Denied, Expired, Failed, Queued, Received, Running,
};

/// The statuses a request in `from` may move to.
pub fn next_statuses(from: RequestStatus) -> &'static [RequestStatus] {
    match from {
        Received => &[Queued, Canceled, Failed],
        Queued => &[Running, Canceled, Failed],
        Running => &[AwaitingApproval, Completed, Failed],
        AwaitingApproval => &[Running, Denied, Expired, Failed],
        Completed | Denied | Failed | Canceled | Expired => &[],
    }
}

pub fn can_transition(from: RequestStatus, to: RequestStatus) -> bool {
    next_statuses(from).contains(&to)
}

/// `conflict` when the machine does not allow the move.
pub fn check_transition(from: RequestStatus, to: RequestStatus) -> Result<(), ApiError> {
    if can_transition(from, to) {
        Ok(())
    } else {
        Err(ApiError::new(
            ErrorCode::Conflict,
            format!(
                "a {} request cannot become {}",
                status_name(from),
                status_name(to)
            ),
        ))
    }
}

/// The wire name, for error messages.
pub fn status_name(status: RequestStatus) -> &'static str {
    match status {
        Received => "received",
        AwaitingApproval => "awaiting_approval",
        Queued => "queued",
        Running => "running",
        Completed => "completed",
        Denied => "denied",
        Failed => "failed",
        Canceled => "canceled",
        Expired => "expired",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [RequestStatus; 9] = [
        Received,
        AwaitingApproval,
        Queued,
        Running,
        Completed,
        Denied,
        Failed,
        Canceled,
        Expired,
    ];

    #[test]
    fn names_match_the_wire_format() {
        for status in ALL {
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::json!(status_name(status))
            );
        }
    }

    #[test]
    fn only_the_documented_moves_are_allowed() {
        let allowed = [
            (Received, Queued),
            (Received, Canceled),
            (Received, Failed),
            (Queued, Running),
            (Queued, Canceled),
            (Queued, Failed),
            (Running, AwaitingApproval),
            (Running, Completed),
            (Running, Failed),
            (AwaitingApproval, Running),
            (AwaitingApproval, Denied),
            (AwaitingApproval, Expired),
            (AwaitingApproval, Failed),
        ];
        for from in ALL {
            for to in ALL {
                let expected = allowed.contains(&(from, to));
                assert_eq!(can_transition(from, to), expected, "{from:?} -> {to:?}");
                assert_eq!(check_transition(from, to).is_ok(), expected);
            }
        }
    }

    #[test]
    fn final_statuses_never_change_and_others_can_finish() {
        for status in ALL {
            assert_eq!(next_statuses(status).is_empty(), status.is_final());
            if !status.is_final() {
                assert!(
                    can_transition(status, Failed),
                    "{status:?} must be able to fail"
                );
            }
        }
    }

    #[test]
    fn invalid_moves_are_conflicts() {
        let error = check_transition(Running, Canceled).unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(error.message, "a running request cannot become canceled");
        assert!(check_transition(Completed, Running).is_err());
        assert!(check_transition(Queued, Completed).is_err());
        assert!(check_transition(Received, AwaitingApproval).is_err());
        assert!(check_transition(Expired, Expired).is_err());
    }
}
