//! State store — bounded ring of events with monotonic sequence.
//!
//! Mirrors herdr's `EventHub` pattern: `Arc<Mutex<ring + seq>>`
//! with `events_after(seq)` pull for client catch-up.

use mc_schema::events::EventEnvelope;

#[derive(Clone)]
pub struct StateStore {
    inner: std::sync::Arc<std::sync::Mutex<StateStoreInner>>,
}

struct StateStoreInner {
    next_sequence: u64,
    events: Vec<(u64, EventEnvelope)>,
}

impl StateStore {
    const MAX_EVENTS: usize = 512;

    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(StateStoreInner {
                next_sequence: 0,
                events: Vec::new(),
            })),
        }
    }

    pub fn push(&self, event: EventEnvelope) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        state.next_sequence += 1;
        let sequence = state.next_sequence;
        state.events.push((sequence, event));
        let overflow = state.events.len().saturating_sub(Self::MAX_EVENTS);
        if overflow > 0 {
            state.events.drain(0..overflow);
        }
    }

    pub fn events_after(
        &self,
        sequence: u64,
    ) -> Vec<(u64, EventEnvelope)> {
        let Ok(state) = self.inner.lock() else {
            return Vec::new();
        };
        state
            .events
            .iter()
            .filter(|(event_sequence, _)| *event_sequence > sequence)
            .cloned()
            .collect()
    }

    pub fn current_sequence(&self) -> u64 {
        let Ok(state) = self.inner.lock() else {
            return 0;
        };
        state.next_sequence
    }
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}