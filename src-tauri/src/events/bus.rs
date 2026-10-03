//! In-process event bus with two lanes, like the web's `utils/event_bus.py`.
//!
//! Each subscriber gets its own bounded queue and its own worker task, so:
//! * a slow subscriber (an alert sender) never delays another (the log);
//! * events reach a subscriber in publish order;
//! * memory is bounded: a full best-effort queue sheds the event (sampled
//!   warning), a full critical queue logs an error every time. The critical
//!   cap is ten times larger, for subscribers on the money path.
//!
//! `publish` never blocks and never awaits. Workers are owned by the bus in a
//! `JoinSet`; `shutdown` closes the queues, lets workers drain for a short
//! grace period, then aborts what is left.

use super::{Event, Topic};
use futures_util::FutureExt;
use parking_lot::{Mutex, RwLock};
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

pub const BEST_EFFORT_CAP: usize = 1000;
pub const CRITICAL_CAP: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    BestEffort,
    Critical,
}

#[async_trait::async_trait]
pub trait Subscriber: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Topics this subscriber receives.
    fn topics(&self) -> Vec<Topic>;
    async fn handle(&self, event: Arc<Event>);
}

struct Registered {
    name: &'static str,
    topics: HashSet<Topic>,
    lane: Lane,
    tx: mpsc::Sender<Arc<Event>>,
}

#[derive(Debug, Default)]
pub struct BusStats {
    pub published: AtomicU64,
    pub dropped: AtomicU64,
    pub critical_dropped: AtomicU64,
}

pub struct EventBus {
    subs: RwLock<Vec<Registered>>,
    tasks: Mutex<JoinSet<()>>,
    stats: Arc<BusStats>,
    best_effort_cap: usize,
    critical_cap: usize,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self::with_caps(BEST_EFFORT_CAP, CRITICAL_CAP)
    }

    pub fn with_caps(best_effort_cap: usize, critical_cap: usize) -> Self {
        Self {
            subs: RwLock::new(Vec::new()),
            tasks: Mutex::new(JoinSet::new()),
            stats: Arc::new(BusStats::default()),
            best_effort_cap: best_effort_cap.max(1),
            critical_cap: critical_cap.max(1),
        }
    }

    pub fn stats(&self) -> &BusStats {
        &self.stats
    }

    /// Register a subscriber. Must be called from inside a Tokio runtime.
    pub fn subscribe(&self, sub: Arc<dyn Subscriber>, lane: Lane) {
        let cap = match lane {
            Lane::BestEffort => self.best_effort_cap,
            Lane::Critical => self.critical_cap,
        };
        let (tx, mut rx) = mpsc::channel::<Arc<Event>>(cap);
        let name = sub.name();
        let worker = sub.clone();
        self.tasks.lock().spawn(async move {
            while let Some(ev) = rx.recv().await {
                let topic = ev.topic().as_str();
                if AssertUnwindSafe(worker.handle(ev))
                    .catch_unwind()
                    .await
                    .is_err()
                {
                    tracing::error!("Event subscriber '{}' failed on '{}'", name, topic);
                }
            }
        });
        self.subs.write().push(Registered {
            name,
            topics: sub.topics().into_iter().collect(),
            lane,
            tx,
        });
        tracing::debug!("EventBus: subscribed '{}' ({:?} lane)", name, lane);
    }

    /// Publish without blocking. Returns the number of subscribers reached.
    pub fn publish(&self, event: Event) -> usize {
        self.stats.published.fetch_add(1, Ordering::Relaxed);
        let topic = event.topic();
        let ev = Arc::new(event);
        let subs = self.subs.read();
        let mut delivered = 0;
        for s in subs.iter().filter(|s| s.topics.contains(&topic)) {
            match s.tx.try_send(ev.clone()) {
                Ok(()) => delivered += 1,
                Err(mpsc::error::TrySendError::Full(_)) => match s.lane {
                    Lane::BestEffort => {
                        let n = self.stats.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                        if n == 1 || n.is_multiple_of(100) {
                            tracing::warn!(
                                "EventBus at capacity; dropped '{}' for '{}'. {} dropped since start.",
                                topic.as_str(),
                                s.name,
                                n
                            );
                        }
                    }
                    Lane::Critical => {
                        let n = self.stats.critical_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                        tracing::error!(
                            "EventBus critical lane full; '{}' did not receive '{}'. {} critical drops since start.",
                            s.name,
                            topic.as_str(),
                            n
                        );
                    }
                },
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
        delivered
    }

    /// Close every queue, give workers `grace` to drain, abort the rest.
    pub async fn shutdown(&self, grace: Duration) {
        self.subs.write().clear();
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        let drained =
            tokio::time::timeout(grace, async { while tasks.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subs.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SessionEndReason;
    use tokio::sync::Notify;

    struct Recorder {
        seen: Mutex<Vec<String>>,
        gate: Option<Arc<Notify>>,
    }

    #[async_trait::async_trait]
    impl Subscriber for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn topics(&self) -> Vec<Topic> {
            vec![Topic::ForceLogout]
        }
        async fn handle(&self, event: Arc<Event>) {
            if let Some(g) = &self.gate {
                g.notified().await;
            }
            if let Event::ForceLogout { message } = &*event {
                self.seen.lock().push(message.clone());
            }
        }
    }

    fn ev(i: usize) -> Event {
        Event::ForceLogout {
            message: i.to_string(),
        }
    }

    #[tokio::test]
    async fn delivers_in_publish_order() {
        let bus = EventBus::new();
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: None,
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        for i in 0..200 {
            bus.publish(ev(i));
        }
        bus.shutdown(Duration::from_secs(5)).await;
        let seen = r.seen.lock().clone();
        assert_eq!(seen, (0..200).map(|i| i.to_string()).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn only_subscribed_topics_are_delivered() {
        let bus = EventBus::new();
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: None,
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        assert_eq!(
            bus.publish(Event::BrokerSessionEnded {
                reason: SessionEndReason::Logout
            }),
            0
        );
        assert_eq!(bus.publish(ev(1)), 1);
        bus.shutdown(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn best_effort_lane_sheds_when_full() {
        let bus = EventBus::with_caps(4, 100);
        let gate = Arc::new(Notify::new());
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: Some(gate.clone()),
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        for i in 0..20 {
            bus.publish(ev(i));
            tokio::task::yield_now().await;
        }
        let dropped = bus.stats().dropped.load(Ordering::Relaxed);
        // Queue holds 4, at most one more is in the worker's hands.
        assert!(dropped >= 15, "dropped {}", dropped);
        assert_eq!(bus.stats().critical_dropped.load(Ordering::Relaxed), 0);
        bus.shutdown(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn critical_lane_never_drops_below_cap() {
        let cap = 500;
        let bus = EventBus::with_caps(4, cap);
        let gate = Arc::new(Notify::new());
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: Some(gate.clone()),
        });
        bus.subscribe(r.clone(), Lane::Critical);
        for i in 0..cap {
            bus.publish(ev(i));
        }
        assert_eq!(bus.stats().critical_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(bus.stats().dropped.load(Ordering::Relaxed), 0);
        // Release the worker; everything published is delivered in order.
        for _ in 0..cap {
            gate.notify_one();
            tokio::task::yield_now().await;
        }
        for _ in 0..50 {
            if r.seen.lock().len() == cap {
                break;
            }
            gate.notify_one();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(r.seen.lock().len(), cap);
        bus.shutdown(Duration::from_secs(1)).await;
    }

    struct Panicker;
    #[async_trait::async_trait]
    impl Subscriber for Panicker {
        fn name(&self) -> &'static str {
            "panicker"
        }
        fn topics(&self) -> Vec<Topic> {
            vec![Topic::ForceLogout]
        }
        async fn handle(&self, _event: Arc<Event>) {
            panic!("boom");
        }
    }

    #[tokio::test]
    async fn a_panicking_subscriber_keeps_its_worker() {
        let bus = EventBus::new();
        bus.subscribe(Arc::new(Panicker), Lane::BestEffort);
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: None,
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        bus.publish(ev(1));
        bus.publish(ev(2));
        bus.shutdown(Duration::from_secs(5)).await;
        assert_eq!(r.seen.lock().len(), 2);
    }
}
