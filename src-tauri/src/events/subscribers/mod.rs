//! Subscribers registered at startup (web `subscribers/__init__.py`).

pub mod log;
pub mod socketio;

use super::{EventBus, Lane};
use crate::db::sqlite::logs::LogsDb;
use std::sync::Arc;

pub use socketio::{SocketEmitter, UiEmitter};

/// Wire every subscriber to the bus. Call once, inside the runtime.
pub fn register_all(bus: &EventBus, logs: Arc<LogsDb>, ui: Arc<dyn UiEmitter>) {
    bus.subscribe(Arc::new(log::LogSubscriber::new(logs)), Lane::BestEffort);
    bus.subscribe(
        Arc::new(socketio::SocketIoSubscriber::new(ui)),
        Lane::BestEffort,
    );
}
