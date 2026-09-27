//! Shared harness for the integration tests.
//!
//! Everything runs in-process on `127.0.0.1:0`: no root, no network, no fixed
//! port. Each test gets its own temporary state directory.

#![allow(dead_code)]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use icu_proto::{
    com::{ComKey, Side},
    error as ec, framing,
    header::{VIDEO_HEADER_LEN, VIDEO_MAX_FRAME},
    AudioHeader, ControlMsg, Session, UserFlags, UserId, VideoHeader, MAX_CONTROL_FRAME,
};
use icu_server::{config::Config, AdminRequest, AdminResponse, Handle};
use quinn::{ClientConfig, Endpoint, RecvStream, SendStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// Default timeout for "a message must arrive".
pub const SOON: Duration = Duration::from_secs(5);
/// Timeout for "nothing must arrive".
pub const BRIEFLY: Duration = Duration::from_millis(300);

/// A running server plus its temporary state directory.
pub struct TestServer {
    pub dir: tempfile::TempDir,
    pub handle: Handle,
}

impl TestServer {
    /// Starts a server with the default test configuration.
    pub async fn start() -> TestServer {
        TestServer::start_with(|_| {}).await
    }

    /// Starts a server, letting the caller adjust the configuration first.
    pub async fn start_with(adjust: impl FnOnce(&mut Config)) -> TestServer {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::for_test(dir.path());
        adjust(&mut config);
        let handle = icu_server::run(config).await.expect("server starts");
        TestServer { dir, handle }
    }

    /// The bound address.
    pub fn addr(&self) -> SocketAddr {
        self.handle.local_addrs()[0]
    }

    /// The CA certificate.
    pub fn ca(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.handle.ca_der())
    }

    /// The com key every datagram to this server must carry.
    pub fn com_key(&self) -> ComKey {
        self.handle.server().com_key.clone()
    }

    /// The admin socket path.
    pub fn socket(&self) -> std::path::PathBuf {
        self.handle
            .admin_socket()
            .expect("the admin socket is up")
            .to_path_buf()
    }

    /// Issues a registration bundle through the admin path. `label` is the
    /// placeholder name; empty means a generated one.
    pub async fn invite(&self, label: &str) -> String {
        match self
            .handle
            .admin(AdminRequest::Invite {
                name: label.to_string(),
                ttl_s: Some(3600),
            })
            .await
        {
            AdminResponse::Invite { bundle, .. } => bundle,
            other => panic!("invite failed: {other:?}"),
        }
    }

    /// Full registration: bundle, connect with it, swap it for a certificate
    /// on a fresh key carrying `name`.
    pub async fn enroll(&self, name: &str) -> Identity {
        let bundle = self.invite("").await;
        self.register(&bundle, name)
            .await
            .expect("registration succeeds")
    }

    /// Registers with an explicit bundle, so the failure paths are testable.
    /// The name travels in the CSR's subject CN (`PROTOCOL.md` §2.3).
    pub async fn register(&self, bundle: &str, name: &str) -> Result<Identity, EnrollFail> {
        let (key, csr) = csr_with_cn(name);
        self.register_with_csr(bundle, key, csr).await
    }

    /// [`TestServer::register`] with a caller-built CSR.
    pub async fn register_with_csr(
        &self,
        bundle: &str,
        key: rcgen::KeyPair,
        csr: Vec<u8>,
    ) -> Result<Identity, EnrollFail> {
        self.register_closing(bundle, key, csr, ec::REGISTRATION_STORED)
            .await
    }

    /// Registers like [`TestServer::register`] but ends the session with
    /// `SHUTDOWN` — anything but `REGISTRATION_STORED` is a client that did
    /// not confirm it stored the certificate (`PROTOCOL.md` §2.3).
    pub async fn register_unconfirmed(
        &self,
        bundle: &str,
        name: &str,
    ) -> Result<Identity, EnrollFail> {
        let (key, csr) = csr_with_cn(name);
        self.register_closing(bundle, key, csr, ec::SHUTDOWN).await
    }

    /// Registers and ends the session with `close_code`.
    pub async fn register_closing(
        &self,
        bundle: &str,
        key: rcgen::KeyPair,
        csr: Vec<u8>,
        close_code: u32,
    ) -> Result<Identity, EnrollFail> {
        let registration = identity_from_bundle(bundle);
        let registration_fp = icu_server::store::fingerprint(&registration.cert);
        let mut client = self
            .connect(&registration)
            .await
            .map_err(EnrollFail::Connection)?;
        let welcome = client
            .hello_with("", icu_proto::MINOR, UserFlags::NONE)
            .await
            .ok_or_else(|| {
                EnrollFail::Connection(match client.conn.close_reason() {
                    Some(e) => e.to_string(),
                    None => "no Welcome".into(),
                })
            })?;
        let ControlMsg::Welcome { registration, .. } = welcome else {
            panic!("expected Welcome, got {welcome:?}");
        };
        assert!(
            registration,
            "a registration certificate gets a registration session"
        );
        client
            .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
            .await;
        let answer = client
            .recv_matching(|m| !matches!(m, ControlMsg::Pong { .. }))
            .await;
        let result = match answer {
            Some(ControlMsg::RenewResponse { leaf_der }) => Ok(Identity {
                cert: CertificateDer::from(leaf_der),
                key_pem: key.serialize_pem(),
                key_der: key.serialize_der(),
                ca: registration_ca(bundle),
            }),
            Some(ControlMsg::Error { code, detail }) => Err(EnrollFail::Rejected { code, detail }),
            Some(other) => panic!("unexpected registration answer: {other:?}"),
            None => Err(EnrollFail::Connection(match client.conn.close_reason() {
                Some(e) => e.to_string(),
                None => "no answer".into(),
            })),
        };
        client.conn.close(quinn::VarInt::from_u32(close_code), b"");
        // The server finishes the session after the close — the receipt is
        // a store write — so wait for that: whatever the test does next
        // sees a finished registration, not one in progress.
        let roomless = &self.handle.server().roomless;
        for _ in 0..200 {
            if !roomless
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&registration_fp)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        result
    }

    /// Enrolls and immediately connects and says `Hello`.
    pub async fn client(&self, name: &str) -> Client {
        let identity = self.enroll(name).await;
        let mut client = self.connect(&identity).await.expect("connect");
        client.hello(name).await;
        client
    }

    /// Connects with an existing identity, without saying `Hello`.
    pub async fn connect(&self, identity: &Identity) -> Result<Client, String> {
        connect_with(
            self.addr(),
            self.ca(),
            &self.com_key(),
            Some(identity.clone()),
        )
        .await
    }

    /// Connects with `identity` and returns the code the connection ends
    /// with, handshake alert or application close, whichever comes.
    pub async fn rejection(&self, identity: &Identity) -> Option<u32> {
        let endpoint = client_endpoint(
            self.ca(),
            &self.com_key(),
            Some(identity.clone()),
            icu_proto::limits::ALPN_MAIN,
        );
        let conn = match endpoint.connect(self.addr(), "localhost").ok()?.await {
            Ok(conn) => conn,
            Err(e) => return code_of(&e),
        };
        let _ = conn.open_bi().await;
        let reason = tokio::time::timeout(SOON, conn.closed()).await.ok()?;
        code_of(&reason)
    }

    /// Connects without a client certificate.
    pub async fn connect_anonymous(&self) -> Result<Client, String> {
        connect_with(self.addr(), self.ca(), &self.com_key(), None).await
    }

    /// Shuts the server down.
    pub async fn shutdown(self) {
        self.handle.shutdown().await;
    }

    /// Stops the server and starts it again on the same state directory,
    /// letting the caller adjust the configuration. The port changes.
    pub async fn restart_with(self, adjust: impl FnOnce(&mut Config)) -> TestServer {
        let TestServer { dir, handle } = self;
        handle.shutdown().await;
        let mut config = Config::for_test(dir.path());
        adjust(&mut config);
        let handle = icu_server::run(config).await.expect("server restarts");
        TestServer { dir, handle }
    }
}

/// A CSR asking for `cn` on an existing identity's key: a renewal.
pub fn renewal_csr(identity: &Identity, cn: &str) -> Vec<u8> {
    let key = rcgen::KeyPair::from_pem(&identity.key_pem).expect("identity key");
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn);
    params.serialize_request(&key).expect("csr").der().to_vec()
}

/// A PKCS#10 CSR asking for `cn`, plus the key that signed it. The one place
/// these tests put a name into a certificate request.
pub fn csr_with_cn(cn: &str) -> (rcgen::KeyPair, Vec<u8>) {
    let key = rcgen::KeyPair::generate().expect("keypair");
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn);
    let csr = params.serialize_request(&key).expect("csr").der().to_vec();
    (key, csr)
}

/// Why a registration failed.
#[derive(Debug)]
pub enum EnrollFail {
    /// The server answered with `Error`.
    Rejected {
        /// Error code.
        code: u32,
        /// Detail text.
        detail: String,
    },
    /// The connection failed or was closed.
    Connection(String),
}

impl EnrollFail {
    /// The error code, whether it arrived as a message or as a close code.
    pub fn code(&self) -> Option<u32> {
        match self {
            EnrollFail::Rejected { code, .. } => Some(*code),
            EnrollFail::Connection(text) => close_code_from(text),
        }
    }
}

/// The identity inside a registration bundle.
pub fn identity_from_bundle(bundle: &str) -> Identity {
    let parts = icu_proto::passport::Passport::decode(bundle).expect("bundle decodes");
    let key = rcgen::KeyPair::try_from(parts.key_der.as_slice()).expect("bundle key");
    Identity {
        cert: CertificateDer::from(parts.leaf_der),
        key_pem: key.serialize_pem(),
        key_der: parts.key_der,
        ca: CertificateDer::from(parts.ca_der),
    }
}

fn registration_ca(bundle: &str) -> CertificateDer<'static> {
    identity_from_bundle(bundle).ca
}

/// A client certificate plus its key.
#[derive(Debug, Clone)]
pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key_pem: String,
    pub key_der: Vec<u8>,
    pub ca: CertificateDer<'static>,
}

impl Identity {
    /// The private key in the form rustls wants.
    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}

/// A connected test client with its control stream.
pub struct Client {
    pub endpoint: Endpoint,
    pub conn: quinn::Connection,
    pub send: SendStream,
    pub recv: RecvStream,
    pub session: Session,
    pub user_id: UserId,
    /// The newest `CrlUpdate` seen; the receive helpers take it out of the
    /// stream, since it can arrive at any time.
    pub crl: Option<Vec<u8>>,
}

impl Client {
    /// Sends `Hello` and consumes the `Welcome`.
    pub async fn hello(&mut self, name: &str) -> ControlMsg {
        self.hello_with(name, icu_proto::MINOR, UserFlags::NONE)
            .await
            .expect("Welcome")
    }

    /// `Hello` with explicit minor and flags. Returns `None` when the server
    /// closed instead of answering.
    pub async fn hello_with(
        &mut self,
        name: &str,
        minor: u16,
        initial: UserFlags,
    ) -> Option<ControlMsg> {
        self.hello_full(name, minor, "icu-test/0.1.0 linux", initial)
            .await
    }

    /// `Hello` with an explicit `client` string, for the `min_client` check.
    pub async fn hello_as(&mut self, name: &str, client: &str) -> Option<ControlMsg> {
        self.hello_full(name, icu_proto::MINOR, client, UserFlags::NONE)
            .await
    }

    async fn hello_full(
        &mut self,
        name: &str,
        minor: u16,
        client: &str,
        initial: UserFlags,
    ) -> Option<ControlMsg> {
        self.send_msg(&ControlMsg::Hello {
            minor,
            caps: 0,
            client: client.to_string(),
            initial,
            name: name.to_string(),
        })
        .await;
        let welcome = self.recv_timeout(SOON).await?;
        if let ControlMsg::Welcome { you, .. } = &welcome {
            self.session = you.session;
            self.user_id = you.user_id;
        }
        Some(welcome)
    }

    /// Sends a control message.
    pub async fn send_msg(&mut self, msg: &ControlMsg) {
        let frame = msg.encode().expect("encode");
        self.send.write_all(&frame).await.expect("write");
    }

    /// Sends raw bytes on the control stream, for framing tests.
    pub async fn send_raw(&mut self, bytes: &[u8]) {
        let _ = self.send.write_all(bytes).await;
    }

    /// Waits for the next control message.
    pub async fn recv_msg(&mut self) -> ControlMsg {
        self.recv_timeout(SOON)
            .await
            .expect("a control message within the timeout")
    }

    /// Waits at most `timeout` for the next control message.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Option<ControlMsg> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let body =
                tokio::time::timeout_at(deadline, read_frame(&mut self.recv, MAX_CONTROL_FRAME))
                    .await
                    .ok()?
                    .ok()?;
            match ControlMsg::decode(&body).ok()? {
                ControlMsg::CrlUpdate { crl_der } => self.crl = Some(crl_der),
                msg => return Some(msg),
            }
        }
    }

    /// Waits for the next message of a kind the predicate accepts, skipping
    /// anything else.
    pub async fn recv_matching(
        &mut self,
        mut accept: impl FnMut(&ControlMsg) -> bool,
    ) -> Option<ControlMsg> {
        let deadline = tokio::time::Instant::now() + SOON;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            let msg = self.recv_timeout(left).await?;
            if accept(&msg) {
                return Some(msg);
            }
        }
    }

    /// Asserts that nothing arrives for a short while, other than the
    /// periodic `UserRtt` background broadcast (every ~2 s whenever the room
    /// has 2+ participants) — expected noise, not a signal any test cares
    /// about.
    pub async fn expect_silence(&mut self) {
        let deadline = tokio::time::Instant::now() + BRIEFLY;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return;
            }
            match self.recv_timeout(left).await {
                Some(ControlMsg::UserRtt { .. }) => continue,
                Some(msg) => panic!("expected silence, got {msg:?}"),
                None => return,
            }
        }
    }

    /// Sends one audio datagram with `speaker = 0`, as a client must.
    pub fn send_audio(&self, seq: u16, ts: u32, payload: &[u8]) {
        self.send_audio_flags(seq, ts, payload, 0);
    }

    /// Sends one audio datagram with explicit flags.
    pub fn send_audio_flags(&self, seq: u16, ts: u32, payload: &[u8], flags: u8) {
        let header = AudioHeader {
            flags,
            speaker: 0,
            seq,
            ts,
        };
        let datagram = header.datagram(payload).expect("datagram");
        let _ = self.conn.send_datagram(datagram.into());
    }

    /// Waits for the next audio datagram.
    pub async fn recv_audio(&self, timeout: Duration) -> Option<(AudioHeader, Vec<u8>)> {
        let datagram = tokio::time::timeout(timeout, self.conn.read_datagram())
            .await
            .ok()?
            .ok()?;
        let (header, payload) = AudioHeader::decode(&datagram).ok()?;
        Some((header, payload.to_vec()))
    }

    /// Sends one video frame on its own unidirectional stream.
    pub async fn send_video(&self, seq: u16, ts: u32, keyframe: bool, payload: &[u8]) {
        let header = VideoHeader {
            flags: if keyframe {
                icu_proto::header::VIDEO_FLAG_KEYFRAME
            } else {
                0
            },
            speaker: 0,
            seq,
            ts,
            codec: icu_proto::header::VIDEO_CODEC_H264,
        };
        let mut frame = header.encode().to_vec();
        frame.extend_from_slice(payload);
        let mut send = self.conn.open_uni().await.expect("open_uni");
        send.write_all(&frame).await.expect("write video");
        let _ = send.finish();
    }

    /// Waits for the next video frame.
    pub async fn recv_video(&self, timeout: Duration) -> Option<(VideoHeader, Vec<u8>)> {
        let mut recv = tokio::time::timeout(timeout, self.conn.accept_uni())
            .await
            .ok()?
            .ok()?;
        let frame = recv
            .read_to_end(VIDEO_HEADER_LEN + VIDEO_MAX_FRAME)
            .await
            .ok()?;
        let header = VideoHeader::decode(&frame).ok()?;
        Some((header, frame[VIDEO_HEADER_LEN..].to_vec()))
    }

    /// Waits until the connection is closed and returns the close code.
    pub async fn closed_code(&self) -> Option<u32> {
        let reason = tokio::time::timeout(SOON, self.conn.closed()).await.ok()?;
        code_of(&reason)
    }
}

/// QUIC's code for the TLS alert `certificate_revoked` (0x100 + 44): what a
/// certificate on the server's CRL gets during the handshake.
pub const TLS_CERTIFICATE_REVOKED: u32 = 0x100 + 44;

/// The application close code of a connection error, or `0x100 + alert` for
/// a TLS alert.
pub fn code_of(error: &quinn::ConnectionError) -> Option<u32> {
    match error {
        quinn::ConnectionError::ApplicationClosed(closed) => {
            Some(u64::from(closed.error_code) as u32)
        }
        quinn::ConnectionError::ConnectionClosed(closed)
            if (0x100..0x200).contains(&u64::from(closed.error_code)) =>
        {
            Some(u64::from(closed.error_code) as u32)
        }
        _ => None,
    }
}

fn close_code_from(text: &str) -> Option<u32> {
    // "closed by peer: code 3" / "connection closed: ..." — the tests that
    // need the exact code use `closed_code()`; this is only a fallback.
    let idx = text.find("code ")?;
    text[idx + 5..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Builds a client endpoint that trusts exactly `ca` and tags its datagrams
/// with `com_key`.
pub fn client_endpoint(
    ca: CertificateDer<'static>,
    com_key: &ComKey,
    identity: Option<Identity>,
    alpn: &[u8],
) -> Endpoint {
    let mut endpoint = bare_endpoint(com_key);
    endpoint.set_default_client_config(client_config(ca, identity, alpn));
    endpoint
}

/// A client endpoint on `127.0.0.1` without a default configuration,
/// speaking with `com_key`'s tag (`PROTOCOL.md` §1).
pub fn bare_endpoint(com_key: &ComKey) -> Endpoint {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind 127.0.0.1");
    icu_proto::com::endpoint(socket, com_key, Side::Client, None, Arc::default())
        .expect("client endpoint")
}

/// The client configuration of [`client_endpoint`], for an endpoint built
/// some other way.
pub fn client_config(
    ca: CertificateDer<'static>,
    identity: Option<Identity>,
    alpn: &[u8],
) -> ClientConfig {
    client_config_with_chain(
        ca,
        identity.map(|identity| (vec![identity.cert.clone()], identity.key())),
        alpn,
    )
}

/// [`client_config`] presenting `auth`'s certificate chain as it is — the
/// leaf first — with its key.
pub fn client_config_with_chain(
    ca: CertificateDer<'static>,
    auth: Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
    alpn: &[u8],
) -> ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).expect("add ca");
    // Always build from an explicit provider: several servers and clients live
    // in one test binary, and a process-wide default provider would race.
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls13")
    .with_root_certificates(roots);
    let mut tls = match auth {
        Some((chain, key)) => builder
            .with_client_auth_cert(chain, key)
            .expect("client auth"),
        None => builder.with_no_client_auth(),
    };
    tls.alpn_protocols = vec![alpn.to_vec()];

    let mut config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config"),
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(
        icu_proto::limits::KEEP_ALIVE_INTERVAL_S,
    )));
    transport.max_idle_timeout(Some(
        Duration::from_secs(icu_proto::limits::MAX_IDLE_TIMEOUT_S)
            .try_into()
            .unwrap(),
    ));
    transport.datagram_receive_buffer_size(Some(icu_proto::limits::DATAGRAM_BUFFER_BYTES));
    transport.datagram_send_buffer_size(icu_proto::limits::CLIENT_DATAGRAM_SEND_BUFFER_BYTES);
    transport.max_concurrent_uni_streams(quinn::VarInt::from_u32(
        icu_proto::limits::max_uni_streams(icu_proto::limits::DEFAULT_MAX_USERS),
    ));
    config.transport_config(Arc::new(transport));
    config
}

/// Connects on ALPN `icu/1` and opens the control stream.
pub async fn connect_with(
    addr: SocketAddr,
    ca: CertificateDer<'static>,
    com_key: &ComKey,
    identity: Option<Identity>,
) -> Result<Client, String> {
    let endpoint = client_endpoint(ca, com_key, identity, icu_proto::limits::ALPN_MAIN);
    let conn = endpoint
        .connect(addr, "localhost")
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    let (send, recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    Ok(Client {
        endpoint,
        conn,
        send,
        recv,
        session: 0,
        user_id: 0,
        crl: None,
    })
}

/// Reads one length prefixed frame.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>, String> {
    let mut prefix = [0u8; framing::LEN_PREFIX];
    recv.read_exact(&mut prefix)
        .await
        .map_err(|e| e.to_string())?;
    let len = framing::frame_len(prefix);
    framing::check_len(len, max).map_err(|e| e.to_string())?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(body)
}

/// Whether this process runs as root, which reads through every mode bit
/// a test sets up; says so when it skips.
pub fn root() -> bool {
    let root = rustix::process::geteuid().is_root();
    if root {
        eprintln!("skipped: root is not held back by file modes");
    }
    root
}

/// Sets a file's mode and puts the old one back when dropped, so the
/// temporary directory goes away even after a failed assertion.
pub struct Mode(std::path::PathBuf, u32);

impl Mode {
    pub fn set(path: &std::path::Path, mode: u32) -> Mode {
        use std::os::unix::fs::PermissionsExt;
        let old = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        Mode(path.to_path_buf(), old)
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}
