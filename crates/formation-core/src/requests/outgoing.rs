//! What leaves the owner's Mac about a work request. The requester sees the
//! result and error text only after secret masking (NFR-003), the same masking
//! answers get; the owner's own view keeps the original.

use formation_proto::request::WorkRequest;

use crate::ask::mask::Masker;

/// The request as its requester may see it: `result` and the error message
/// masked, everything else unchanged.
pub fn for_requester(request: &WorkRequest) -> WorkRequest {
    let masker = Masker::new();
    let mut out = request.clone();
    out.result = out.result.as_deref().map(|text| masker.mask(text));
    if let Some(error) = out.error.as_mut() {
        error.message = masker.mask(&error.message);
    }
    out
}
