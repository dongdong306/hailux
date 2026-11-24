//! Session-level event bus: fanout layer from background runs to SSE subscribers.
//!
//! One bus per session (indexed by session_id via `BusRegistry`):
//! - **Monotonic seq**: one seq per event, continuous across runs (reconnect
//!   dedup baseline)
//! - **Replay buffer**: all events of the current run; retained after the run
//!   ends until the next run starts, so reconnecting clients catch up the tail
//!   with their `since` cursor
//! - **durable watermark**: advanced by the runner at each persistence point
//!   (PersistMessage / AgentComplete / CompactComplete); `head_seq` =
//!   durable_seq, making DB history and replay complementary without overlap
//! - **epoch**: server start timestamp; a change means the seq space is
//!   invalidated and clients must rebuild from DB history
//!
//! Attach protocol (no loss, no dup): `subscribe()` first, then take the
//! `replay_since()` snapshot, then skip live events with seq <= replayed.
//!
//! Memory: the buffer holds the whole run (tool results truncated to 100k
//! chars before publish) plus the tail until the next run starts.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::broadcast;

use super::protocol::{Envelope, ServerEvent};

/// Recover from lock poisoning: the only source is a panic while holding the
/// lock; reusing the inner data beats paralyzing the event plane.
fn lock_or_recover<T, R>(mutex: &Mutex<T>, f: impl FnOnce(&mut T) -> R) -> R {
    let mut guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)
}

fn rwlock_or_recover<T, R>(rwlock: &RwLock<T>, f: impl FnOnce(&mut T) -> R) -> R {
    let mut guard = rwlock.write().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running,
}

pub struct SessionBus {
    epoch: u64,
    next_seq: AtomicU64,
    durable_seq: AtomicU64,
    buffer: Mutex<Vec<Envelope>>,
    tx: broadcast::Sender<Envelope>,
    state: RwLock<RunState>,
    /// Run claim slot (0 = idle). CAS-claimed by chat/compact handlers so two
    /// concurrent POSTs cannot both spawn (the `is_running` check alone has a
    /// race window); the claim covers a spawned runner still queued on the
    /// session lock, during which new requests get 409 — otherwise an observer
    /// would attach to someone else's run and never see its own.
    claims: AtomicUsize,
}

impl SessionBus {
    pub fn new(epoch: u64) -> Self {
        // Capacity need not cover a whole run (thousands of events): a lagging
        // subscriber gets Lagged, the endpoint terminates its stream, and the
        // client reconnects with its cursor to replay from the buffer.
        // 1024 balances memory (viewed sessions all get a bus, ~100B/slot).
        let (tx, _) = broadcast::channel(1024);
        Self {
            epoch,
            next_seq: AtomicU64::new(0),
            durable_seq: AtomicU64::new(0),
            buffer: Mutex::new(Vec::new()),
            tx,
            state: RwLock::new(RunState::Idle),
            claims: AtomicUsize::new(0),
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn state(&self) -> RunState {
        rwlock_or_recover(&self.state, |st| *st)
    }

    pub fn is_running(&self) -> bool {
        self.state() == RunState::Running
    }

    /// Run start: drop the previous run's replay buffer (kept until now for
    /// reconnect catch-up) and mark the bus Running.
    pub fn begin_run(&self) {
        lock_or_recover(&self.buffer, |buf| buf.clear());
        rwlock_or_recover(&self.state, |st| *st = RunState::Running);
    }

    /// Run end (incl. trailing compaction): mark Idle; the buffer is retained
    /// for reconnect catch-up.
    pub fn end_run(&self) {
        rwlock_or_recover(&self.state, |st| *st = RunState::Idle);
    }

    /// Try to claim the run slot (CAS 0 -> 1). Only one run per session at a
    /// time, including queued runners; false means already claimed and the
    /// request is rejected with 409.
    pub fn try_claim_run(&self) -> bool {
        self.claims
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Release the run claim (runner exit, including abnormal paths).
    pub fn release_run_claim(&self) {
        self.claims.store(0, Ordering::Release);
    }

    /// Publish an event: allocate seq, append to the replay buffer, broadcast.
    pub fn publish(&self, event: ServerEvent) -> Envelope {
        // Seq starts at 1; 0 means "no events" (initial head_seq).
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let env = Envelope { seq, event };
        lock_or_recover(&self.buffer, |buf| buf.push(env.clone()));
        // Err = no subscribers; the replay buffer covers late attachers.
        let _ = self.tx.send(env.clone());
        env
    }

    /// Advance the durable watermark (monotonic; called after the DB writes
    /// covering all events up to `seq` completed).
    pub fn advance_durable(&self, seq: u64) {
        let mut cur = self.durable_seq.load(Ordering::Relaxed);
        while seq > cur {
            match self.durable_seq.compare_exchange_weak(
                cur,
                seq,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn durable_seq(&self) -> u64 {
        self.durable_seq.load(Ordering::Relaxed)
    }

    /// Seq of the most recently published event; the runner advances the
    /// durable watermark to this after its DB writes complete.
    pub fn last_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Relaxed)
    }

    /// Subscribe to live events (attach protocol: subscribe first, replay
    /// second — see module docs).
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.tx.subscribe()
    }

    /// Snapshot of buffered events with seq > since.
    pub fn replay_since(&self, since: u64) -> Vec<Envelope> {
        lock_or_recover(&self.buffer, |buf| {
            buf.iter()
                .filter(|e| e.seq > since)
                .cloned()
                .collect::<Vec<_>>()
        })
    }
}

/// session_id -> SessionBus registry.
#[derive(Default)]
pub struct BusRegistry {
    inner: Mutex<HashMap<String, Arc<SessionBus>>>,
}

impl BusRegistry {
    pub fn get(&self, session_id: &str) -> Option<Arc<SessionBus>> {
        lock_or_recover(&self.inner, |map| map.get(session_id).cloned())
    }

    /// Get or create (the epoch is fixed at creation; constant per process).
    pub fn get_or_create(&self, session_id: &str, epoch: u64) -> Arc<SessionBus> {
        lock_or_recover(&self.inner, |map| {
            map.entry(session_id.to_string())
                .or_insert_with(|| Arc::new(SessionBus::new(epoch)))
                .clone()
        })
    }

    /// Session ids currently in Running state (sidebar live indicator).
    pub fn running_ids(&self) -> HashSet<String> {
        lock_or_recover(&self.inner, |map| {
            map.iter()
                .filter(|(_, bus)| bus.is_running())
                .map(|(k, _)| k.clone())
                .collect::<HashSet<_>>()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::protocol::ServerEvent;

    fn make_bus() -> SessionBus {
        SessionBus::new(12345)
    }

    #[test]
    fn seq_increases_monotonically_and_buffer_keeps_all() {
        let bus = make_bus();
        let e1 = bus.publish(ServerEvent::Notice { text: "a".into() });
        let e2 = bus.publish(ServerEvent::Notice { text: "b".into() });
        assert_eq!((e1.seq, e2.seq), (1, 2));

        let all = bus.replay_since(0);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 1);
        assert_eq!(all[1].seq, 2);
    }

    #[test]
    fn replay_since_filters_by_cursor() {
        let bus = make_bus();
        bus.publish(ServerEvent::Notice { text: "a".into() });
        bus.publish(ServerEvent::Notice { text: "b".into() });
        bus.publish(ServerEvent::Notice { text: "c".into() });

        let tail = bus.replay_since(2);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 3);
        assert_eq!(tail[0].event, ServerEvent::Notice { text: "c".into() });
    }

    #[test]
    fn begin_run_clears_buffer_but_seq_continues() {
        let bus = make_bus();
        bus.publish(ServerEvent::Notice { text: "old".into() });
        bus.begin_run();
        assert!(
            bus.replay_since(0).is_empty(),
            "buffer cleared at run start"
        );

        let env = bus.publish(ServerEvent::Notice { text: "new".into() });
        assert_eq!(env.seq, 2, "seq stays continuous across runs");
    }

    #[test]
    fn durable_seq_only_advances() {
        let bus = make_bus();
        bus.advance_durable(5);
        assert_eq!(bus.durable_seq(), 5);
        bus.advance_durable(3); // regression is a no-op
        assert_eq!(bus.durable_seq(), 5);
        bus.advance_durable(9);
        assert_eq!(bus.durable_seq(), 9);
    }

    #[test]
    fn run_state_transitions() {
        let bus = make_bus();
        assert!(!bus.is_running());
        bus.begin_run();
        assert!(bus.is_running());
        bus.end_run();
        assert!(!bus.is_running());
        // buffer is retained after the run ends
        bus.begin_run();
        let env = bus.publish(ServerEvent::Notice { text: "x".into() });
        bus.end_run();
        assert!(!bus.replay_since(0).is_empty());
        assert_eq!(env.seq, 1);
    }

    #[test]
    fn subscribe_receives_published_events() {
        let bus = make_bus();
        let mut rx = bus.subscribe();
        bus.publish(ServerEvent::Notice {
            text: "live".into(),
        });
        let received = rx.try_recv().unwrap();
        assert_eq!(received.seq, 1);
    }

    #[test]
    fn registry_creates_and_reuses_buses() {
        let registry = BusRegistry::default();
        let b1 = registry.get_or_create("s1", 100);
        let b1_again = registry.get_or_create("s1", 100);
        assert!(Arc::ptr_eq(&b1, &b1_again));
        assert_eq!(b1.epoch(), 100);
        assert!(registry.get("s2").is_none());

        assert!(registry.running_ids().is_empty());
        b1.begin_run();
        assert_eq!(registry.running_ids(), ["s1".into()].into_iter().collect());
        b1.end_run();
        assert!(registry.running_ids().is_empty());
    }

    #[test]
    fn run_claim_is_exclusive_and_released() {
        let bus = make_bus();
        assert!(bus.try_claim_run());
        assert!(!bus.try_claim_run(), "second claim must fail while held");
        bus.release_run_claim();
        assert!(bus.try_claim_run());
        bus.release_run_claim();
    }
}
