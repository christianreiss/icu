//! The ICU voice server as a library.
//!
//! [`run`] starts everything and hands back a [`Handle`]; the binary in
//! `main.rs` is a thin clap wrapper around it. Running in-process is what
//! makes the integration tests (and later the client's tests) possible:
//! bind `127.0.0.1:0`, no root, no network.
//!
//! ```no_run
//! # async fn example() -> icu_server::Result<()> {
//! let dir = std::path::Path::new("/tmp/icu-state");
//! let config = icu_server::Config::for_test(dir);
//! let handle = icu_server::run(config).await?;
//! let addr = handle.local_addrs()[0];
//! let ca = handle.ca_der();
//! handle.shutdown().await;
//! # let _ = (addr, ca);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod admin;
pub mod ca;
pub mod cli;
pub mod config;
pub mod endpoint;
pub mod enroll;
pub mod error;
pub mod invite_key;
pub mod limits;
pub mod loss;
pub mod relay;
pub mod room;
pub mod session;
pub mod store;
pub mod wire;

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{sync::watch, task::JoinHandle};

pub use crate::{
    admin::{AdminRequest, AdminResponse},
    config::Config,
    error::{Error, Result},
};

use crate::{
    ca::Ca,
    invite_key::InviteKey,
    limits::IpLimits,
    loss::LossTracker,
    room::{Counters, Room},
    store::Store,
};

/// Everything the connection tasks share.
pub struct Server {
    /// Validated configuration.
    pub config: Config,
    /// CA and server certificate. Behind a lock because
    /// `rotate-server-cert` replaces the leaf at runtime.
    pub ca: tokio::sync::RwLock<Ca>,
    /// Persistent state. The running server is its only writer (§5).
    pub store: tokio::sync::Mutex<Store>,
    /// Seals/opens the bearer token carried inside an invite blob. Never
    /// rotates in v1, so no lock is needed.
    pub invite_key: InviteKey,
    /// The one room.
    pub room: Room,
    /// Relay counters.
    pub counters: Counters,
    /// Per-IP rate limits.
    pub ip_limits: std::sync::Mutex<IpLimits>,
    /// `LossReport` aggregation per speaker.
    pub loss: std::sync::Mutex<LossTracker>,
    /// Last forwarded `KeyframeRequest` per speaker.
    pub keyframes: std::sync::Mutex<std::collections::HashMap<icu_proto::Session, Instant>>,
    /// Flipped to `true` on shutdown.
    pub shutdown: watch::Sender<bool>,
    started: Instant,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("online", &self.room.len())
            .field("uptime_s", &self.uptime().as_secs())
            .finish_non_exhaustive()
    }
}

impl Server {
    /// How long this process has been serving.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// Builds a server that owns the state but serves nothing, for CLI
    /// commands running while no server is up (§5).
    pub fn offline(config: Config) -> Result<Arc<Server>> {
        config.validate()?;
        let store = Store::open(&config.paths.state_dir)?;
        let ca = Ca::load_or_create(
            &config.paths.state_dir,
            &config.tls,
            &config.server.server_name,
        )?;
        let invite_key = InviteKey::load_or_create(&config.paths.state_dir)?;
        let (shutdown, _) = watch::channel(false);
        Ok(Arc::new(Server {
            room: Room::new(config.server.max_users),
            ip_limits: std::sync::Mutex::new(IpLimits::new(&config.limits)),
            loss: std::sync::Mutex::new(LossTracker::default()),
            keyframes: std::sync::Mutex::new(std::collections::HashMap::new()),
            counters: Counters::default(),
            ca: tokio::sync::RwLock::new(ca),
            store: tokio::sync::Mutex::new(store),
            invite_key,
            started: Instant::now(),
            shutdown,
            config,
        }))
    }

    /// Resolves when shutdown is requested.
    pub async fn shutdown_requested(&self) {
        let mut rx = self.shutdown.subscribe();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// A running server.
pub struct Handle {
    server: Arc<Server>,
    addrs: Vec<SocketAddr>,
    endpoints: Vec<quinn::Endpoint>,
    tasks: Vec<JoinHandle<()>>,
    admin_socket: Option<std::path::PathBuf>,
}

impl Handle {
    /// The addresses actually bound. With `port = 0` this is where the
    /// ephemeral port shows up.
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// The CA certificate in DER form. A client trusts exactly this.
    pub fn ca_der(&self) -> Vec<u8> {
        self.server
            .ca
            .try_read()
            .map(|ca| ca.ca_der().to_vec())
            .unwrap_or_default()
    }

    /// The shared state, for tests and for the admin socket.
    pub fn server(&self) -> &Arc<Server> {
        &self.server
    }

    /// Path of the admin socket, if one could be created.
    pub fn admin_socket(&self) -> Option<&std::path::Path> {
        self.admin_socket.as_deref()
    }

    /// Runs one admin command through exactly the code path the Unix socket
    /// uses. This is how an in-process test mints an invite token.
    pub async fn admin(&self, request: AdminRequest) -> AdminResponse {
        admin::handle(&self.server, request).await
    }

    /// Closes all connections with `SHUTDOWN`, waits the grace period and
    /// releases the state lock (§6.1.8).
    pub async fn shutdown(self) {
        let _ = self.server.shutdown.send(true);
        self.server
            .room
            .mark_all_closing(icu_proto::error::SHUTDOWN);
        for conn in self.server.room.connections() {
            conn.close(
                quinn::VarInt::from_u32(icu_proto::error::SHUTDOWN),
                b"shutdown",
            );
        }
        tokio::time::sleep(Duration::from_millis(
            self.server.config.timing.shutdown_grace_ms,
        ))
        .await;
        for endpoint in &self.endpoints {
            endpoint.close(
                quinn::VarInt::from_u32(icu_proto::error::SHUTDOWN),
                b"shutdown",
            );
        }
        for task in &self.tasks {
            task.abort();
        }
        if let Some(path) = &self.admin_socket {
            let _ = std::fs::remove_file(path);
        }
        // Dropping the Arc releases the Store and with it `flock(state.lock)`,
        // but only once every task holding a clone is gone.
        for endpoint in &self.endpoints {
            endpoint.wait_idle().await;
        }
    }
}

/// Starts the server: state, CA, endpoints, accept loops, admin socket.
///
/// Returns as soon as everything is bound, so a caller can read
/// [`Handle::local_addrs`] straight away.
pub async fn run(config: Config) -> Result<Handle> {
    config.validate()?;

    // 1–4: state directory, lock, CA, server certificate.
    let mut store = Store::open(&config.paths.state_dir)?;
    let purged = store.purge_expired_invites()?;
    if purged > 0 {
        tracing::info!(purged, "dropped expired invites");
    }
    let ca = Ca::load_or_create(
        &config.paths.state_dir,
        &config.tls,
        &config.server.server_name,
    )?;
    let invite_key = InviteKey::load_or_create(&config.paths.state_dir)?;

    // 5–7: TLS, QUIC, one endpoint per listen address.
    let quic_config = endpoint::server_config(&config, &ca)?;
    let mut endpoints = Vec::new();
    let mut addrs = Vec::new();
    for addr in &config.server.listen {
        let endpoint = endpoint::bind(*addr, quic_config.clone())?;
        addrs.push(endpoint.local_addr()?);
        endpoints.push(endpoint);
    }

    let (shutdown, _) = watch::channel(false);
    let server = Arc::new(Server {
        room: Room::new(config.server.max_users),
        ip_limits: std::sync::Mutex::new(IpLimits::new(&config.limits)),
        loss: std::sync::Mutex::new(LossTracker::default()),
        keyframes: std::sync::Mutex::new(std::collections::HashMap::new()),
        counters: Counters::default(),
        ca: tokio::sync::RwLock::new(ca),
        store: tokio::sync::Mutex::new(store),
        invite_key,
        started: Instant::now(),
        shutdown,
        config,
    });

    let mut tasks = Vec::new();
    for endpoint in &endpoints {
        tasks.push(tokio::spawn(endpoint::accept_loop(
            server.clone(),
            endpoint.clone(),
        )));
    }
    tasks.push(tokio::spawn(housekeeping(server.clone())));

    // 8: admin socket. Not being able to create it is not fatal — the server
    // still serves; only the CLI is then unavailable (deviation, see report).
    let admin_socket = match admin::listen(server.clone()).await {
        Ok((path, task)) => {
            tasks.push(task);
            Some(path)
        }
        Err(e) => {
            tracing::warn!(error = %e, "admin socket unavailable");
            None
        }
    };

    tracing::info!(
        addrs = ?addrs,
        max_users = server.config.server.max_users,
        "icu-server listening"
    );

    Ok(Handle {
        server,
        addrs,
        endpoints,
        tasks,
        admin_socket,
    })
}

/// Periodic work: expiring talk spurts, expiring `LossReport`s, pruning IP
/// buckets.
async fn housekeeping(server: Arc<Server>) {
    let silence = server.config.timing.speaking_silence();
    let ttl = server.config.timing.loss_report_ttl();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut prune_counter = 0u32;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = server.shutdown_requested() => return,
        }

        for (session, flags) in server.room.expire_speaking(silence) {
            server
                .room
                .broadcast(&icu_proto::ControlMsg::UserState { session, flags });
        }

        // The expiry of a report is a change: the speaker must learn that it
        // can stop paying for FEC (`PROTOCOL.md` §2.2).
        let hints = {
            let mut loss = server.loss.lock().unwrap_or_else(|e| e.into_inner());
            loss.expire(ttl)
        };
        for (speaker, loss_permille) in hints {
            server
                .room
                .send_to(speaker, &icu_proto::ControlMsg::LossHint { loss_permille });
        }

        prune_counter += 1;

        // Every 2 s (20 ticks of 100 ms): server-measured RTT per participant
        // (`PROTOCOL.md` §2.2 `UserRtt`), broadcast to the whole room.
        if prune_counter.is_multiple_of(20) {
            let rtts = server.room.rtt_snapshot();
            if !rtts.is_empty() {
                server
                    .room
                    .broadcast(&icu_proto::ControlMsg::UserRtt { rtts });
            }
        }

        if prune_counter.is_multiple_of(600) {
            server
                .ip_limits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .prune();
        }
    }
}

/// Installs the `tracing` subscriber from the configuration.
pub fn init_tracing(log: &config::LogSection) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log.level.clone()));
    let builder = fmt::Subscriber::builder().with_env_filter(filter);
    let result = if log.format == "json" {
        tracing::subscriber::set_global_default(builder.json().finish())
    } else {
        tracing::subscriber::set_global_default(builder.finish())
    };
    let _ = result;
}
