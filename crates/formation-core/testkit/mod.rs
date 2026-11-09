//! A's offline adapters for integration tests and early development.
mod harness;
mod store;

pub use harness::{FakeHarness, HarnessCall, ScriptedAgent, ScriptedPage};
pub use store::MemoryStore;

// B's fakes of the collaboration ports (TASK-042).
pub mod collab;
pub use collab::*;

use std::sync::Mutex;

use formation_proto::{common::Seq, events::Event};

/// Records publications atomically, in sequence order.
#[derive(Default)]
pub struct RecordingEventSink {
    events: Mutex<Vec<(Seq, Event)>>,
}

impl RecordingEventSink {
    pub fn events(&self) -> Vec<(Seq, Event)> {
        self.events.lock().unwrap().clone()
    }
}

impl crate::ports::EventSink for RecordingEventSink {
    fn publish(&self, event: Event) -> Seq {
        let mut events = self.events.lock().unwrap();
        let seq = Seq(events.len() as u64 + 1);
        events.push((seq, event));
        seq
    }
}
