//! Harness for the client integration tests: an in-process server,
//! registration through the real client code, and a raw control client for
//! the server side of a conversation.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use icu_client::{
    audio::AudioBackend,
    config::{Identity, Paths, Settings},
    engine::{AppState, ConnStatus, Engine, EngineConfig},
    net::{
        conn::{self, Session},
        register::register,
    },
};
use icu_proto::ControlMsg;
use icu_server::{config::Config, AdminRequest, AdminResponse, Handle};

/// How long a state transition may take.
pub const SOON: Duration = Duration::from_secs(8);

/// A running server plus its state directory.
pub struct TestServer {
    pub dir: tempfile::TempDir,
    pub handle: Handle,
}

impl TestServer {
    pub async fn start() -> TestServer {
        TestServer::start_with_com_key(None).await
    }

    /// A new server — new CA — that keeps `com_key`, as if only `com.key`
    /// had come back from a backup; `None` for a fresh key.
    pub async fn start_with_com_key(com_key: Option<&icu_proto::com::ComKey>) -> TestServer {
        // `RUST_LOG=icu_client::dsp::jitter=debug` traces the jitter
        // buffers; without the variable nothing is installed.
        if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_test_writer()
                .try_init();
        }
        let dir = tempfile::tempdir().expect("tempdir");
        if let Some(key) = com_key {
            std::fs::write(
                dir.path().join(icu_server::store::COM_KEY_FILE),
                format!("{}\n", key.to_base64()),
            )
            .expect("com.key");
        }
        let config = Config::for_test(dir.path());
        let handle = icu_server::run(config).await.expect("server starts");
        TestServer { dir, handle }
    }

    /// Restarts on the same address with the same state directory.
    pub async fn restart(self, addr: SocketAddr) -> TestServer {
        let TestServer { dir, handle } = self;
        handle.shutdown().await;
        let mut config = Config::for_test(dir.path());
        config.server.listen = vec![addr];
        let mut last_err = None;
        for _ in 0..50 {
            match icu_server::run(config.clone()).await {
                Ok(handle) => return TestServer { dir, handle },
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        panic!("server restart failed: {last_err:?}");
    }

    pub fn addr(&self) -> SocketAddr {
        self.handle.local_addrs()[0]
    }

    /// Issues a registration bundle; `name` is the placeholder.
    pub async fn invite_bundle(&self, name: &str) -> String {
        match self
            .handle
            .admin(AdminRequest::Invite {
                name: name.to_string(),
                ttl_s: Some(3600),
            })
            .await
        {
            AdminResponse::Invite { bundle, .. } => bundle,
            other => panic!("invite failed: {other:?}"),
        }
    }

    /// Registers through the real client code into a fresh config directory.
    pub async fn enroll(&self, name: &str) -> (Identity, Paths, tempfile::TempDir) {
        let bundle = Identity::from_passport(&self.invite_bundle("").await).expect("bundle");
        assert!(bundle.is_registration());
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let identity = register(
            &bundle,
            name,
            "icu-test/0.1.0 linux",
            Some(self.addr()),
            &paths,
        )
        .await
        .expect("registration");
        assert_eq!(
            Identity::load(&paths)
                .expect("load")
                .expect("stored")
                .leaf_der,
            identity.leaf_der,
            "register stores the identity itself"
        );
        (identity, paths, dir)
    }

    /// Starts an engine for `name` with `backend`.
    pub async fn engine(
        &self,
        name: &str,
        backend: Box<dyn AudioBackend>,
        test_tone: bool,
    ) -> (Engine, tempfile::TempDir) {
        let (engine, _paths, dir) = self.engine_with_paths(name, backend, test_tone).await;
        (engine, dir)
    }

    /// Like [`TestServer::engine`], but hands back the config directory too —
    /// what a test needs to look at the certificate on disk.
    pub async fn engine_with_paths(
        &self,
        name: &str,
        backend: Box<dyn AudioBackend>,
        test_tone: bool,
    ) -> (Engine, Paths, tempfile::TempDir) {
        let (identity, paths, dir) = self.enroll(name).await;
        self.engine_from(identity, paths.clone(), backend, test_tone)
            .map(|engine| (engine, paths, dir))
            .expect("engine starts")
    }

    /// Like [`TestServer::engine`], but connecting through `via` (a
    /// [`JitterProxy`] in front of this server).
    pub async fn engine_via(
        &self,
        name: &str,
        backend: Box<dyn AudioBackend>,
        test_tone: bool,
        via: SocketAddr,
    ) -> (Engine, tempfile::TempDir) {
        let (identity, paths, dir) = self.enroll(name).await;
        let engine = engine_at(identity, paths, backend, test_tone, via).expect("engine starts");
        (engine, dir)
    }

    /// An engine on an identity the caller already has (passport import, a
    /// stale certificate, a second device).
    pub fn engine_from(
        &self,
        identity: Identity,
        paths: Paths,
        backend: Box<dyn AudioBackend>,
        test_tone: bool,
    ) -> Result<Engine, String> {
        engine_at(identity, paths, backend, test_tone, self.addr())
    }

    /// A raw control client (no engine) that has said `Hello`.
    pub async fn raw_client(&self, name: &str) -> RawClient {
        let (identity, _paths, _dir) = self.enroll(name).await;
        let (config, cert_failure) = conn::client_config(&identity).expect("client config");
        let endpoint =
            icu_client::net::bind_endpoint(self.addr(), &identity.com_key).expect("endpoint");
        let session = conn::connect(
            &endpoint,
            config,
            self.addr(),
            "localhost",
            "icu-test/0.1.0 linux",
            // Stands in for a peer with its window open: without `VIEWING`
            // the server relays no video to it and senders pause.
            icu_proto::UserFlags(icu_proto::UserFlags::VIEWING),
            name,
            &cert_failure,
        )
        .await
        .expect("connect");
        RawClient {
            endpoint,
            session,
            _dir,
        }
    }
}

/// The settings every test engine runs with: no AEC and no denoiser (a fake
/// backend has nothing to cancel, and RNNoise would eat the test tone), and
/// no URI handler touching the desktop.
pub fn test_settings() -> Settings {
    let mut settings = Settings::default();
    settings.audio.aec = false;
    settings.audio.denoise = false;
    settings.ui.register_uri_handler = false;
    settings
}

/// An engine on `identity` dialing `addr` instead of the identity's host.
pub fn engine_at(
    identity: Identity,
    paths: Paths,
    backend: Box<dyn AudioBackend>,
    test_tone: bool,
    addr: SocketAddr,
) -> Result<Engine, String> {
    let mut cfg = EngineConfig::new(identity, test_settings(), paths);
    cfg.server_addr = Some(addr);
    cfg.test_tone = test_tone;
    Engine::start(cfg, backend).map_err(|e| e.to_string())
}

/// A raw client for driving the server from "the other side".
pub struct RawClient {
    pub endpoint: quinn::Endpoint,
    pub session: Session,
    _dir: tempfile::TempDir,
}

impl RawClient {
    pub async fn send(&mut self, msg: &ControlMsg) {
        conn::write_msg(&mut self.session.send, msg)
            .await
            .expect("send");
    }

    pub async fn recv(&mut self, timeout: Duration) -> Option<ControlMsg> {
        let body = tokio::time::timeout(
            timeout,
            icu_client::net::read_frame(&mut self.session.recv, icu_proto::MAX_CONTROL_FRAME),
        )
        .await
        .ok()?
        .ok()?;
        ControlMsg::decode(&body).ok()
    }

    pub async fn recv_matching(
        &mut self,
        timeout: Duration,
        mut accept: impl FnMut(&ControlMsg) -> bool,
    ) -> Option<ControlMsg> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            let msg = self.recv(left).await?;
            if accept(&msg) {
                return Some(msg);
            }
        }
    }

    pub async fn recv_video(&self, timeout: Duration) -> Option<(icu_proto::VideoHeader, Vec<u8>)> {
        let mut recv = tokio::time::timeout(timeout, self.session.conn.accept_uni())
            .await
            .ok()?
            .ok()?;
        let frame = recv
            .read_to_end(icu_proto::header::VIDEO_HEADER_LEN + icu_proto::header::VIDEO_MAX_FRAME)
            .await
            .ok()?;
        let header = icu_proto::VideoHeader::decode(&frame).ok()?;
        Some((
            header,
            frame[icu_proto::header::VIDEO_HEADER_LEN..].to_vec(),
        ))
    }
}

/// Waits until `pred` holds for the engine state.
pub async fn wait_for(
    engine: &Engine,
    timeout: Duration,
    mut pred: impl FnMut(&AppState) -> bool,
) -> AppState {
    let mut rx = engine.state();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        {
            let s = rx.borrow_and_update().clone();
            if pred(&s) {
                return s;
            }
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            panic!("timed out waiting for state, last: {:?}", rx.borrow().conn);
        }
        let _ = tokio::time::timeout(left, rx.changed()).await;
    }
}

/// Waits for `Connected`.
pub async fn wait_connected(engine: &Engine) -> AppState {
    wait_for(engine, SOON, |s| s.conn == ConnStatus::Connected).await
}

/// Waits until `pred` holds, polling every 50 ms.
pub async fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pred()
}

/// A UDP relay that delays packets like a busy Wi-Fi link: a few ms of
/// jitter on every packet, and now and then a stall that holds everything
/// back and then releases it in one burst. Seeded, so runs compare.
pub struct JitterProxy {
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

/// Delay model of a [`JitterProxy`].
#[derive(Debug, Clone, Copy)]
pub struct Jitter {
    /// Uniform per-packet delay, 0..=`spread`.
    pub spread: Duration,
    /// Chance per packet that a stall starts.
    pub stall_chance: f32,
    /// Stall length range.
    pub stall: (Duration, Duration),
}

struct Delay {
    rng: icu_client::dsp::testsignal::Rng,
    hold_until: tokio::time::Instant,
    model: Jitter,
}

impl Delay {
    fn deliver_at(&mut self) -> tokio::time::Instant {
        let now = tokio::time::Instant::now();
        let u = |r: &mut icu_client::dsp::testsignal::Rng| (r.uniform() + 1.0) / 2.0;
        if u(&mut self.rng) < self.model.stall_chance && self.hold_until <= now {
            let (lo, hi) = self.model.stall;
            self.hold_until = now + lo + (hi - lo).mul_f32(u(&mut self.rng));
        }
        let own = now + self.model.spread.mul_f32(u(&mut self.rng));
        own.max(self.hold_until)
    }
}

impl JitterProxy {
    /// Relays to `upstream`, delaying the downstream direction (server →
    /// client) by `model`; upstream packets pass straight through.
    pub async fn start(upstream: SocketAddr, model: Jitter, seed: u64) -> JitterProxy {
        let front = Arc::new(
            tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .expect("proxy bind"),
        );
        let addr = front.local_addr().unwrap();
        let delay = Arc::new(Mutex::new(Delay {
            rng: icu_client::dsp::testsignal::Rng::new(seed),
            hold_until: tokio::time::Instant::now(),
            model,
        }));
        let task = tokio::spawn(async move {
            let mut clients: HashMap<SocketAddr, Arc<tokio::net::UdpSocket>> = HashMap::new();
            let mut buf = vec![0u8; 65_536];
            loop {
                let Ok((n, from)) = front.recv_from(&mut buf).await else {
                    return;
                };
                let up = match clients.get(&from) {
                    Some(up) => up.clone(),
                    None => {
                        let bind = if upstream.is_ipv4() {
                            "127.0.0.1:0"
                        } else {
                            "[::1]:0"
                        };
                        let up = Arc::new(tokio::net::UdpSocket::bind(bind).await.unwrap());
                        up.connect(upstream).await.unwrap();
                        clients.insert(from, up.clone());
                        let (up2, front2, delay) = (up.clone(), front.clone(), delay.clone());
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 65_536];
                            while let Ok(n) = up2.recv(&mut buf).await {
                                let at = delay.lock().unwrap().deliver_at();
                                let (front, pkt) = (front2.clone(), buf[..n].to_vec());
                                tokio::spawn(async move {
                                    tokio::time::sleep_until(at).await;
                                    let _ = front.send_to(&pkt, from).await;
                                });
                            }
                        });
                        up
                    }
                };
                let _ = up.send(&buf[..n]).await;
            }
        });
        JitterProxy { addr, task }
    }
}

impl Drop for JitterProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
