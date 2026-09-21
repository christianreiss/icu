//! Harness for the client integration tests: an in-process server, enrollment
//! through the real client code, and a raw control client for the server side
//! of a conversation.

#![allow(dead_code)]

use std::{net::SocketAddr, time::Duration};

use icu_client::{
    audio::AudioBackend,
    config::{Identity, Paths, Settings},
    engine::{AppState, ConnStatus, Engine, EngineConfig},
    net::{
        conn::{self, Session},
        enroll::{enroll, InviteLink},
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
        let dir = tempfile::tempdir().expect("tempdir");
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

    /// Mints an invite link.
    pub async fn invite_link(&self, name: &str) -> String {
        match self
            .handle
            .admin(AdminRequest::Invite {
                name: name.to_string(),
                ttl_s: Some(3600),
            })
            .await
        {
            AdminResponse::Invite { link, .. } => link,
            other => panic!("invite failed: {other:?}"),
        }
    }

    /// Enrolls through the real client code into a fresh config directory.
    pub async fn enroll(&self, name: &str) -> (Identity, Paths, tempfile::TempDir) {
        let link = InviteLink::parse(&self.invite_link(name).await).expect("link parses");
        let identity = enroll(&link, "", Some(self.addr()))
            .await
            .expect("enrollment");
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        identity.save(&paths).expect("identity saved");
        (identity, paths, dir)
    }

    /// Starts an engine for `name` with `backend`.
    pub async fn engine(
        &self,
        name: &str,
        backend: Box<dyn AudioBackend>,
        test_tone: bool,
    ) -> (Engine, tempfile::TempDir) {
        let (identity, paths, dir) = self.enroll(name).await;
        let mut settings = Settings::default();
        settings.audio.aec = false;
        settings.audio.denoise = false;
        settings.ui.register_uri_handler = false;
        let mut cfg = EngineConfig::new(identity, settings, paths);
        cfg.server_addr = Some(self.addr());
        cfg.test_tone = test_tone;
        let engine = Engine::start(cfg, backend).expect("engine starts");
        (engine, dir)
    }

    /// A raw control client (no engine) that has said `Hello`.
    pub async fn raw_client(&self, name: &str) -> RawClient {
        let (identity, _paths, _dir) = self.enroll(name).await;
        let config = conn::client_config(&identity).expect("client config");
        let endpoint = icu_client::net::bind_endpoint(self.addr()).expect("endpoint");
        let session = conn::connect(
            &endpoint,
            config,
            self.addr(),
            "localhost",
            "icu-test/0.1.0 linux",
            icu_proto::UserFlags::NONE,
            name,
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

/// A raw client for driving the server from "the other side".
pub struct RawClient {
    pub endpoint: quinn::Endpoint,
    pub session: Session,
    _dir: tempfile::TempDir,
}

impl RawClient {
    pub fn session_id(&self) -> icu_proto::Session {
        self.session.welcome.you.session
    }

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
        let frame = recv.read_to_end(1024 * 1024).await.ok()?;
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
