//! Shared harness for the integration tests.
//!
//! Everything runs in-process on `127.0.0.1:0`: no root, no network, no fixed
//! port. Each test gets its own temporary state directory.

#![allow(dead_code)]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use icu_proto::{
    header::VIDEO_HEADER_LEN, AudioHeader, ControlMsg, Session, UserFlags, UserId, VideoHeader,
    MAX_CONTROL_FRAME, MAX_ENROLL_FRAME,
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

    /// Creates an invite token through the admin path.
    pub async fn invite(&self, name: &str) -> String {
        match self
            .handle
            .admin(AdminRequest::Invite {
                name: name.to_string(),
                ttl_s: Some(3600),
            })
            .await
        {
            AdminResponse::Invite { token, .. } => token,
            other => panic!("invite failed: {other:?}"),
        }
    }

    /// Full enrollment over ALPN `icu-enroll/1`, returning a usable identity.
    pub async fn enroll(&self, name: &str) -> Identity {
        let token = self.invite(name).await;
        self.enroll_with_token(&token, name)
            .await
            .expect("enrollment succeeds")
    }

    /// Enrollment with an explicit token, so the failure paths are testable.
    pub async fn enroll_with_token(&self, token: &str, name: &str) -> Result<Identity, EnrollFail> {
        let key = rcgen::KeyPair::generate().expect("keypair");
        let mut params = rcgen::CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "whatever-the-client-wants");
        let csr = params.serialize_request(&key).expect("csr");

        let endpoint = client_endpoint(self.ca(), None, icu_proto::limits::ALPN_ENROLL);
        let conn = endpoint
            .connect(self.addr(), "localhost")
            .expect("connect")
            .await
            .map_err(|e| EnrollFail::Connection(e.to_string()))?;
        let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
        let request = icu_proto::EnrollMsg::EnrollRequest {
            token: token.to_string(),
            csr_der: csr.der().to_vec(),
            name: name.to_string(),
        };
        send.write_all(&request.encode().expect("encode"))
            .await
            .expect("write");
        send.finish().expect("finish");

        let body = read_frame(&mut recv, MAX_ENROLL_FRAME)
            .await
            .map_err(EnrollFail::Connection)?;
        match icu_proto::EnrollMsg::decode(&body).expect("decode") {
            icu_proto::EnrollMsg::EnrollResponse {
                leaf_der, ca_der, ..
            } => Ok(Identity {
                cert: CertificateDer::from(leaf_der),
                key_pem: key.serialize_pem(),
                key_der: key.serialize_der(),
                ca: CertificateDer::from(ca_der),
            }),
            icu_proto::EnrollMsg::Error { code, detail } => {
                Err(EnrollFail::Rejected { code, detail })
            }
            other => panic!("unexpected enrollment answer: {other:?}"),
        }
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
        connect_with(self.addr(), self.ca(), Some(identity.clone())).await
    }

    /// Connects without a client certificate.
    pub async fn connect_anonymous(&self) -> Result<Client, String> {
        connect_with(self.addr(), self.ca(), None).await
    }

    /// Shuts the server down.
    pub async fn shutdown(self) {
        self.handle.shutdown().await;
    }
}

/// Why an enrollment failed.
#[derive(Debug)]
pub enum EnrollFail {
    /// The server answered with `EnrollMsg::Error`.
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
        self.send_msg(&ControlMsg::Hello {
            minor,
            caps: 0,
            client: "icu-test/0.1.0 linux".into(),
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
        let body = tokio::time::timeout(timeout, read_frame(&mut self.recv, MAX_CONTROL_FRAME))
            .await
            .ok()?
            .ok()?;
        ControlMsg::decode(&body).ok()
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

    /// Asserts that nothing arrives for a short while.
    pub async fn expect_silence(&mut self) {
        if let Some(msg) = self.recv_timeout(BRIEFLY).await {
            panic!("expected silence, got {msg:?}");
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
        let frame = recv.read_to_end(1024 * 1024).await.ok()?;
        let header = VideoHeader::decode(&frame).ok()?;
        Some((header, frame[VIDEO_HEADER_LEN..].to_vec()))
    }

    /// Waits until the connection is closed and returns the close code.
    pub async fn closed_code(&self) -> Option<u32> {
        let reason = tokio::time::timeout(SOON, self.conn.closed()).await.ok()?;
        code_of(&reason)
    }
}

/// The application close code of a connection error, if any.
pub fn code_of(error: &quinn::ConnectionError) -> Option<u32> {
    match error {
        quinn::ConnectionError::ApplicationClosed(closed) => {
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

/// Builds a client endpoint that trusts exactly `ca`.
pub fn client_endpoint(
    ca: CertificateDer<'static>,
    identity: Option<Identity>,
    alpn: &[u8],
) -> Endpoint {
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
    let mut tls = match identity {
        Some(identity) => builder
            .with_client_auth_cert(vec![identity.cert.clone()], identity.key())
            .expect("client auth"),
        None => builder.with_no_client_auth(),
    };
    tls.alpn_protocols = vec![alpn.to_vec()];

    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("client endpoint");
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
    transport.datagram_send_buffer_size(icu_proto::limits::DATAGRAM_BUFFER_BYTES);
    transport.max_concurrent_uni_streams(quinn::VarInt::from_u32(36));
    config.transport_config(Arc::new(transport));
    endpoint.set_default_client_config(config);
    endpoint
}

/// Connects on ALPN `icu/1` and opens the control stream.
pub async fn connect_with(
    addr: SocketAddr,
    ca: CertificateDer<'static>,
    identity: Option<Identity>,
) -> Result<Client, String> {
    let endpoint = client_endpoint(ca, identity, icu_proto::limits::ALPN_MAIN);
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
    })
}

/// Reads one length prefixed frame.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>, String> {
    let mut prefix = [0u8; 2];
    recv.read_exact(&mut prefix)
        .await
        .map_err(|e| e.to_string())?;
    let len = u16::from_le_bytes(prefix) as usize;
    if len == 0 || len > max {
        return Err(format!("bad frame length {len}"));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(body)
}

/// Extracts the `users` field of a `Welcome`.
pub fn welcome_users(msg: &ControlMsg) -> Vec<icu_proto::UserInfo> {
    match msg {
        ControlMsg::Welcome { users, .. } => users.clone(),
        other => panic!("not a Welcome: {other:?}"),
    }
}
