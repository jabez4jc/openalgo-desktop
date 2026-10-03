//! Application context: the one object every service, HTTP handler and Tauri
//! command receives. It owns the databases, key material, event bus, broker
//! session, web sessions, rate limiter, shared HTTP client, background tasks
//! and the shutdown token.
//!
//! `AppState` is the historical name; `AppContext` is an alias.

use crate::brokers::BrokerRegistry;
use crate::clock::{Clock, SystemClock};
use crate::config::ServerConfig;
use crate::db::duckdb::DuckDb;
use crate::db::sqlite::logs::LogsDb;
use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use crate::events::subscribers::{SocketEmitter, UiEmitter};
use crate::events::EventBus;
use crate::security::keystore::KeyStore;
use crate::security::{Secret, SecurityManager};
use crate::server::ratelimit::RateLimiter;
use crate::services::apikey_service::ApiKeyCache;
use crate::session::web::WebSessionStore;
use crate::websocket::WebSocketManager;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub type AppContext = AppState;

/// Live broker session held in memory while the trader is connected.
#[derive(Debug, Clone)]
pub struct BrokerSession {
    pub broker_id: String,
    pub auth_token: Secret,
    pub feed_token: Option<Secret>,
    pub user_id: String,
    pub user_name: Option<String>,
    pub authenticated_at: DateTime<Utc>,
}

/// Symbol cache entry
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub symbol: String,
    pub token: String,
    pub exchange: String,
    pub name: String,
    pub lot_size: i32,
    pub tick_size: f64,
    pub instrument_type: String,
    /// Broker's original symbol format (e.g., "NSE:RELIANCE-EQ" for Fyers)
    pub brsymbol: Option<String>,
    /// Broker's exchange code
    pub brexchange: Option<String>,
}

/// State of the HTTP listener, shown to the trader when it is not running.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServerStatus {
    Starting,
    Running { host: String, port: u16 },
    PortInUse { port: u16, message: String },
    Failed { message: String },
}

pub struct AppState {
    pub sqlite: Arc<SqliteDb>,
    pub logs: Arc<LogsDb>,
    pub duckdb: Arc<DuckDb>,
    pub security: Arc<SecurityManager>,
    pub brokers: Arc<BrokerRegistry>,
    pub websocket: Arc<WebSocketManager>,
    pub bus: Arc<EventBus>,
    pub ui: Arc<SocketEmitter>,
    pub clock: Arc<dyn Clock>,
    pub config: RwLock<ServerConfig>,
    pub sessions: WebSessionStore,
    pub limiter: RateLimiter,
    pub api_keys: ApiKeyCache,
    pub broker_session: RwLock<Option<BrokerSession>>,
    pub server_status: RwLock<ServerStatus>,
    /// Shared outbound HTTP client (explicit timeouts) for non-broker calls.
    pub http: reqwest::Client,
    pub shutdown: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
    pub symbol_cache: DashMap<String, SymbolInfo>,
    pub symbol_reverse_cache: DashMap<String, String>,
    pub data_dir: PathBuf,
}

pub struct OpenOptions {
    pub keystore: Arc<dyn KeyStore>,
    pub clock: Arc<dyn Clock>,
    pub brokers: Arc<BrokerRegistry>,
}

impl AppState {
    /// Open everything under `data_dir`. Must run inside a Tokio runtime
    /// (subscribers are spawned here).
    pub fn open(data_dir: &Path, opts: OpenOptions) -> Result<Arc<Self>> {
        crate::security::fsperm::ensure_private_dir(data_dir)?;
        let security = Arc::new(SecurityManager::open(data_dir, opts.keystore)?);
        let sqlite = Arc::new(SqliteDb::new(&data_dir.join("openalgo.db"))?);
        let logs = Arc::new(LogsDb::new(&data_dir.join("logs.db"))?);
        {
            let main = sqlite.conn()?;
            logs.import_from_main(&main)?;
        }
        let duck_path = data_dir.join("historify.duckdb");
        let duckdb = Arc::new(DuckDb::new(&duck_path)?);
        crate::security::fsperm::restrict_db_files(&duck_path)?;
        crate::db::sqlite::data_migrations::run(&sqlite, &security)?;
        let config = {
            let conn = sqlite.conn()?;
            ServerConfig::load(&conn)?
        };
        let ui = Arc::new(SocketEmitter::default());
        let bus = Arc::new(EventBus::new());
        crate::events::subscribers::register_all(
            &bus,
            logs.clone(),
            ui.clone() as Arc<dyn UiEmitter>,
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        Ok(Arc::new(Self {
            sqlite,
            logs,
            duckdb,
            security,
            brokers: opts.brokers,
            websocket: Arc::new(WebSocketManager::new()),
            bus,
            ui,
            clock: opts.clock,
            config: RwLock::new(config),
            sessions: WebSessionStore::new(),
            limiter: RateLimiter::new(),
            api_keys: ApiKeyCache::new(),
            broker_session: RwLock::new(None),
            server_status: RwLock::new(ServerStatus::Starting),
            http,
            shutdown: CancellationToken::new(),
            tasks: Mutex::new(JoinSet::new()),
            symbol_cache: DashMap::new(),
            symbol_reverse_cache: DashMap::new(),
            data_dir: data_dir.to_path_buf(),
        }))
    }

    /// Production defaults: OS keychain, system clock, every broker adapter.
    pub fn open_default(data_dir: &Path) -> Result<Arc<Self>> {
        Self::open(
            data_dir,
            OpenOptions {
                keystore: Arc::new(crate::security::keystore::KeyringStore::new()),
                clock: Arc::new(SystemClock),
                brokers: Arc::new(BrokerRegistry::new()),
            },
        )
    }

    /// Spawn a background task owned by the context; aborted on shutdown.
    pub fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        // Reap finished tasks so the set does not grow with one-shot jobs.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    pub fn task_count(&self) -> usize {
        self.tasks.lock().len()
    }

    /// Stop background work: cancel, drain the bus, abort owned tasks,
    /// close the market feed.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.bus.shutdown(Duration::from_secs(2)).await;
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = self.websocket.disconnect().await;
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn server_config(&self) -> ServerConfig {
        self.config.read().clone()
    }

    pub fn reload_config(&self) -> Result<ServerConfig> {
        let c = {
            let conn = self.sqlite.conn()?;
            ServerConfig::load(&conn)?
        };
        *self.config.write() = c.clone();
        Ok(c)
    }

    /// Port the listener is actually bound to (falls back to the setting).
    pub fn listening_port(&self) -> u16 {
        match &*self.server_status.read() {
            ServerStatus::Running { port, .. } => *port,
            _ => self.config.read().http_port,
        }
    }

    /// Broker connected and its token still inside today's session.
    pub fn is_broker_connected(&self) -> bool {
        self.get_broker_session().is_some()
    }

    /// The live broker session, or `None` once it has crossed the daily
    /// boundary (it is then dropped from memory; the scheduler revokes the
    /// stored row).
    pub fn get_broker_session(&self) -> Option<BrokerSession> {
        let s = self.broker_session.read().clone()?;
        let cfg = self.config.read();
        if crate::session::boundary::is_fresh(
            s.authenticated_at,
            self.clock.now(),
            cfg.session_expiry_hour,
            cfg.session_expiry_minute,
        ) {
            Some(s)
        } else {
            drop(cfg);
            *self.broker_session.write() = None;
            None
        }
    }

    pub fn set_broker_session(&self, session: Option<BrokerSession>) {
        *self.broker_session.write() = session;
    }

    /// The signed-in OpenAlgo user, if any (single-user app).
    pub fn signed_in_user(&self) -> Option<String> {
        self.sessions.signed_in_user()
    }

    /// Get symbol info by exchange:token (O(1) lookup)
    pub fn get_symbol_by_token(&self, exchange: &str, token: &str) -> Option<SymbolInfo> {
        let key = format!("{}:{}", exchange, token);
        self.symbol_cache.get(&key).map(|r| r.clone())
    }

    /// Get symbol info by exchange:symbol (O(1) lookup)
    pub fn get_symbol_by_name(&self, exchange: &str, symbol: &str) -> Option<SymbolInfo> {
        let reverse_key = format!("{}:{}", exchange, symbol);
        self.symbol_reverse_cache
            .get(&reverse_key)
            .and_then(|token_ref| {
                let token = token_ref.value();
                let cache_key = format!("{}:{}", exchange, token);
                self.symbol_cache.get(&cache_key).map(|r| r.clone())
            })
    }

    /// Get token by exchange:symbol (O(1) lookup)
    pub fn get_token_by_symbol(&self, exchange: &str, symbol: &str) -> Option<String> {
        let key = format!("{}:{}", exchange, symbol);
        self.symbol_reverse_cache.get(&key).map(|r| r.clone())
    }

    /// Check if symbol exists (O(1) lookup)
    pub fn symbol_exists(&self, exchange: &str, symbol: &str) -> bool {
        let key = format!("{}:{}", exchange, symbol);
        self.symbol_reverse_cache.contains_key(&key)
    }

    /// Get total number of symbols in cache
    pub fn symbol_count(&self) -> usize {
        self.symbol_cache.len()
    }

    /// Replace the symbol cache (bounded by the master contract size; cleared
    /// on logout).
    pub fn load_symbol_cache(&self, symbols: Vec<SymbolInfo>) {
        self.symbol_cache.clear();
        self.symbol_reverse_cache.clear();
        for symbol in symbols {
            let cache_key = format!("{}:{}", symbol.exchange, symbol.token);
            let reverse_key = format!("{}:{}", symbol.exchange, symbol.symbol);
            self.symbol_reverse_cache
                .insert(reverse_key, symbol.token.clone());
            self.symbol_cache.insert(cache_key, symbol);
        }
        tracing::info!("Loaded {} symbols into cache", self.symbol_cache.len());
    }

    pub fn clear_symbol_cache(&self) {
        self.symbol_cache.clear();
        self.symbol_reverse_cache.clear();
    }

    /// Get all symbols for a specific exchange
    pub fn get_symbols_by_exchange(&self, exchange: &str) -> Vec<SymbolInfo> {
        self.symbol_cache
            .iter()
            .filter(|entry| entry.value().exchange.eq_ignore_ascii_case(exchange))
            .map(|entry| entry.value().clone())
            .collect()
    }
}

#[cfg(test)]
pub mod testing {
    //! Context factory for unit and HTTP tests: temp dir, memory keystore,
    //! manual clock, chosen broker adapters.
    use super::*;
    use crate::clock::ManualClock;
    use crate::security::keystore::MemoryKeyStore;

    pub struct TestCtx {
        pub ctx: Arc<AppState>,
        pub clock: Arc<ManualClock>,
        pub dir: tempfile::TempDir,
    }

    pub fn build(brokers: BrokerRegistry, now: DateTime<Utc>) -> TestCtx {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock = ManualClock::new(now);
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
                brokers: Arc::new(brokers),
            },
        )
        .expect("open context");
        TestCtx { ctx, clock, dir }
    }
}
