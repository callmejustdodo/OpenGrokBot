//! fake-formationd serves the M3 mockup's world over formationd's local socket
//! protocol, so the app can be built before the real service exists (TASK-043).
//! See README.md for the states, scripts, and reactions.

pub mod fixtures;
pub mod hub;
