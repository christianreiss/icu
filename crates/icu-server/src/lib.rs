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
pub mod error;
pub mod limits;
pub mod loss;
pub mod registration;
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
    /// The one room.
    pub room: Room,
    /// Relay counters.
    pub counters: Counters,
    /// Per-IP rate limits.
    pub ip_limits: std::sync::Mutex<IpLimits>,
    /// `LossReport` aggregation per speaker.
    pub loss: std::sync::Mutex<LossTracker>,
    /// Handshakes in flight right now, over all endpoints
    /// (`limits.max_handshakes`).
    pub handshakes: std::sync::atomic::AtomicUsize,
    /// Established connections right now, over all endpoints
    /// (`limits.max_connections`).
    pub open_connections: std::sync::atomic::AtomicUsize,
    /// The one live connection per client certificate fingerprint; a newer
    /// one replaces it (`PROTOCOL.md` §7).
    pub live_certs: std::sync::Mutex<std::collections::HashMap<String, quinn::Connection>>,
    /// The QUIC endpoints, so a new CRL or server leaf reaches the TLS
    /// config without a restart (`endpoint::reload_tls`). Empty offline.
    pub endpoints: Vec<quinn::Endpoint>,
    /// The current CRL, DER, as last sent to clients (`CrlUpdate`).
    pub crl: std::sync::RwLock<Vec<u8>>,
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
        Ok(Arc::new(Server::new(
            config,
            store,
            ca,
            Vec::new(),
            Vec::new(),
        )))
    }

    /// The shared state around already opened store, CA and endpoints, with
    /// an empty room and fresh counters.
    fn new(
        config: Config,
        store: Store,
        ca: Ca,
        endpoints: Vec<quinn::Endpoint>,
        crl: Vec<u8>,
    ) -> Server {
        let (shutdown, _) = watch::channel(false);
        Server {
            room: Room::new(config.server.max_users),
            ip_limits: std::sync::Mutex::new(IpLimits::new(&config.limits)),
            loss: std::sync::Mutex::new(LossTracker::default()),
            handshakes: std::sync::atomic::AtomicUsize::new(0),
            open_connections: std::sync::atomic::AtomicUsize::new(0),
            live_certs: std::sync::Mutex::new(std::collections::HashMap::new()),
            endpoints,
            crl: std::sync::RwLock::new(crl),
            counters: Counters::default(),
            ca: tokio::sync::RwLock::new(ca),
            store: tokio::sync::Mutex::new(store),
            started: Instant::now(),
            shutdown,
            config,
        }
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
    /// uses. This is how an in-process test issues a registration bundle.
    pub async fn admin(&self, request: AdminRequest) -> AdminResponse {
        admin::handle(&self.server, request).await
    }

    /// Closes all connections with `SHUTDOWN`, waits the grace period and
    /// releases the state lock (§6.1.8).
    pub async fn shutdown(self) {
        let _ = self.server.shutdown.send(true);
        self.server.room.close_all(icu_proto::error::SHUTDOWN);
        tokio::time::sleep(Duration::from_millis(
            self.server.config.timing.shutdown_grace_ms,
        ))
        .await;
        for endpoint in &self.server.endpoints {
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
        for endpoint in &self.server.endpoints {
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
    let purged = store.purge_expired_registrations()?;
    if purged > 0 {
        tracing::info!(purged, "dropped expired registrations");
    }
    let ca = Ca::load_or_create(
        &config.paths.state_dir,
        &config.tls,
        &config.server.server_name,
    )?;

    // 5–7: TLS, QUIC, one endpoint per listen address.
    // The CRL is rebuilt from the store on every start, so revocations made
    // offline (CLI without a running server) are in force from the first
    // handshake.
    let crl = endpoint::build_crl(&mut store, &ca)?;
    let quic_config = endpoint::server_config(&config, &ca, &crl)?;
    let mut endpoints = Vec::new();
    let mut addrs = Vec::new();
    for addr in &config.server.listen {
        let endpoint = endpoint::bind(*addr, quic_config.clone())?;
        addrs.push(endpoint.local_addr()?);
        endpoints.push(endpoint);
    }

    let server = Arc::new(Server::new(config, store, ca, endpoints, crl.to_vec()));

    let mut tasks = Vec::new();
    for endpoint in &server.endpoints {
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
        let hints = lock(&server.loss).expire(ttl);
        for (speaker, loss_permille) in hints {
            server
                .room
                .send_to(speaker, &icu_proto::ControlMsg::LossHint { loss_permille });
        }

        // Hourly (36 000 ticks of 100 ms), and on the first tick: the server
        // leaf is renewed before it expires, because an expired leaf locks
        // every client out and only the server can fix it.
        if prune_counter.is_multiple_of(36_000) {
            tokio::spawn(check_cert_expiry(server.clone()));
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
            lock(&server.ip_limits).prune();
        }
    }
}

/// Rotates the server leaf once less than `min(30, validity / 3)` (at least
/// one) days are left, and re-issues the CA certificate under the same key a
/// year (at most a third of its validity) before it expires. A certificate
/// that does not parse counts as due.
async fn check_cert_expiry(server: Arc<Server>) {
    const WARN_DAYS: i64 = 30;
    const CA_RENEW_DAYS: i64 = 365;
    let (leaf_left, ca_left) = {
        let ca = server.ca.read().await;
        (
            ca::leaf_days_left_checked(ca.server_der()),
            ca::leaf_days_left_checked(ca.ca_der()),
        )
    };
    if renewal_due(ca_left, server.config.tls.ca_validity_days, CA_RENEW_DAYS) {
        tracing::warn!(days_left = ?ca_left, "the CA certificate expires soon, renewing");
        match admin::renew_ca(&server).await {
            Ok(()) => tracing::info!("CA certificate renewed under the same key"),
            Err(e) => tracing::error!(error = %e, "automatic CA certificate renewal failed"),
        }
    }
    if renewal_due(leaf_left, server.config.tls.server_validity_days, WARN_DAYS) {
        tracing::warn!(
            days_left = ?leaf_left,
            "server certificate expires soon, rotating"
        );
        match admin::rotate_server_cert(&server).await {
            Ok(()) => tracing::info!("server certificate rotated"),
            Err(e) => tracing::error!(error = %e, "automatic server certificate rotation failed"),
        }
    }
}

/// Whether a certificate with `days_left` (`None`: unreadable) is due for
/// renewal: fewer than `min(cap, validity / 3)` days, at least one, remain.
fn renewal_due(days_left: Option<i64>, validity_days: u32, cap_days: i64) -> bool {
    let threshold = cap_days.min(i64::from(validity_days) / 3).max(1);
    days_left.is_none_or(|left| left < threshold)
}

/// Locks a `std` mutex, recovering from poisoning: the state behind every
/// lock in this crate is a plain map or counter that stays consistent when a
/// holder panics, so nothing is gained by propagating the panic.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
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
