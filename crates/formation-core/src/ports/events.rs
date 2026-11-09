//! Owner: A. The service's event bus. Every module publishes its events here; the
//! service numbers them and sends them to connected clients.

use formation_proto::common::Seq;
use formation_proto::events::Event;

pub trait EventSink: Send + Sync {
    /// Publishes the event and returns its sequence number.
    fn publish(&self, event: Event) -> Seq;
}
