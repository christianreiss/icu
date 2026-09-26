//! QUIC endpoint: TLS configuration, binding and the accept loop
//! (`Server/IMPLEMENTATION.md` §6.1.5–§6.2).

use std::{
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use icu_proto::limits as pl;
use quinn::{crypto::rustls::QuicServerConfig, Endpoint, VarInt};
use rustls::{
    client::danger::HandshakeSignatureValid,
    pki_types::{CertificateDer, CertificateRevocationListDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    CertificateError, DigitallySignedStruct, DistinguishedName, RootCertStore, SignatureScheme,
};

use crate::{
    ca::Ca,
    config::Config,
    error::{Context, Error, Result},
    lock,
    store::Store,
    Server,
};

/// Builds the rustls and quinn server configuration.
///
/// The order below is load bearing: `builder_with_provider` yields
/// `WantsVersions`, so `with_protocol_versions` has to come **before** the
/// client certificate verifier (A8). The verifier uses
/// `allow_unauthenticated()` although every client needs a certificate: the
/// requirement is enforced in `session.rs`, which closes with a readable
/// `AUTH_REQUIRED` instead of a bare TLS alert (`PROTOCOL.md` §1, Spike 2b).
///
/// `crl` makes a revoked client certificate fail the handshake itself
/// (`certificate_revoked` alert). Only the leaf is checked; the CA is the
/// issuer of both, so the CRL always speaks for it.
pub fn server_config(
    config: &Config,
    ca: &Ca,
    crl: &CertificateRevocationListDer<'static>,
) -> Result<quinn::ServerConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(ca.ca_der().clone())
        .context("cannot add the ICU CA to the root store")?;

    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .allow_unauthenticated()
    .with_crls([crl.clone()])
    .only_check_end_entity_revocation()
    .build()
    .context("cannot build the client certificate verifier")?;
    let verifier = Arc::new(ExpiredClientCertVerifier(verifier));

    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .context("TLS 1.3 is not available in this provider")?
    .with_client_cert_verifier(verifier)
    .with_single_cert(ca.server_chain(), ca.server_key())
    .context("cannot install the server certificate")?;

    tls.alpn_protocols = vec![pl::ALPN_MAIN.to_vec()];
    // 0-RTT off (`PROTOCOL.md` §1).
    tls.max_early_data_size = 0;
    tls.send_half_rtt_data = false;

    let mut quic = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    quic.migration(true);
    // Connection attempts the accept loop has not decided on yet. It decides
    // at once, so these only fill up in a burst; past them quinn refuses by
    // itself instead of buffering (defaults: 65 536 and 100 MiB).
    quic.max_incoming(INCOMING_QUEUE);
    quic.incoming_buffer_size(64 * 1024);
    quic.incoming_buffer_size_total(4 * 1024 * 1024);

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        Duration::from_millis(config.timing.max_idle_timeout_ms)
            .try_into()
            .map_err(|e| Error::msg(format!("bad max_idle_timeout: {e}")))?,
    ));
    // Asymmetric signatures, see A3: receive takes an Option, send does not.
    transport.datagram_receive_buffer_size(Some(pl::DATAGRAM_BUFFER_BYTES));
    transport.datagram_send_buffer_size(pl::DATAGRAM_BUFFER_BYTES);
    transport.max_concurrent_uni_streams(VarInt::from_u32(pl::max_uni_streams(
        config.server.max_users,
    )));
    // One control stream per connection (`PROTOCOL.md` §1).
    transport.max_concurrent_bidi_streams(VarInt::from_u32(1));
    // Flow control (`PROTOCOL.md` §1). quinn's defaults are 1.25 MB per
    // stream and no limit per connection: a member with 36 video streams
    // open could pin ~47 MB of server memory. Streams nobody reads — a
    // renew-only session never accepts any — count against the connection
    // window as well.
    transport.stream_receive_window(VarInt::from_u32(pl::STREAM_RECEIVE_WINDOW));
    transport.receive_window(VarInt::from_u32(pl::CONNECTION_RECEIVE_WINDOW));
    transport.send_window(pl::SEND_WINDOW);
    quic.transport_config(Arc::new(transport));

    Ok(quic)
}

/// Connection attempts quinn queues for the accept loop before it refuses
/// further ones by itself.
const INCOMING_QUEUE: usize = 1024;

/// Lets an **expired** client certificate through the handshake, so its
/// holder can renew it however long ago it expired (`PROTOCOL.md` §2.3).
/// Refuses a chain longer than the leaf before any check.
///
/// Only expiry is forgiven: when the inner verifier's sole complaint is that
/// the leaf has expired, the leaf is checked again as of one second before its
/// notAfter — chain, signature, key usage and the CRL all still apply. Proof of
/// the key is the TLS handshake itself, and the server is still checked
/// against the CA by the client, so nothing here is trust on first use.
/// Everything past the handshake is `session.rs`: the fingerprint has to be in
/// the store, not revoked, the user not disabled, and an expired leaf only
/// ever gets a renew-only session.
#[derive(Debug)]
struct ExpiredClientCertVerifier(Arc<dyn ClientCertVerifier>);

impl ClientCertVerifier for ExpiredClientCertVerifier {
    fn offer_client_auth(&self) -> bool {
        self.0.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.0.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.0.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        // Clients send their leaf and nothing else: the CA issues leaves
        // directly. Anything more would only be issuer candidates for the
        // path search — signature checks an unauthenticated peer could have
        // the server run by the dozen per handshake.
        if !intermediates.is_empty() {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        match self.0.verify_client_cert(end_entity, intermediates, now) {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::Expired | CertificateError::ExpiredContext { .. },
            )) => {
                let Some(not_after) = crate::ca::leaf_validity(end_entity)
                    .and_then(|(_, not_after)| u64::try_from(not_after.unix_timestamp()).ok())
                else {
                    return Err(rustls::Error::InvalidCertificate(CertificateError::Expired));
                };
                let before =
                    UnixTime::since_unix_epoch(Duration::from_secs(not_after.saturating_sub(1)));
                self.0.verify_client_cert(end_entity, intermediates, before)
            }
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

/// The two CRLs of one reload (`PROTOCOL.md` §1), signed under the same
/// CRL number.
pub struct Crls {
    /// Every revoked certificate the handshake has to refuse: client leaves,
    /// spent and superseded registration and client certificates, replaced
    /// server leaves. Stays in the server.
    pub handshake: CertificateRevocationListDer<'static>,
    /// Only the replaced server leaves, which is all a client checks: what
    /// `CrlUpdate` carries. Revoked client certificates would only grow it
    /// past a control frame, after which clients stopped learning about a
    /// rotated server leaf at all.
    pub clients: CertificateRevocationListDer<'static>,
}

/// Signs fresh CRLs from the store under the next CRL number.
pub fn build_crls(store: &mut Store, ca: &Ca) -> Result<Crls> {
    let number = store.next_crl_number()?;
    Ok(Crls {
        handshake: ca.crl(number, &store.crl_serials())?,
        clients: ca.crl(number, &store.revoked_server_serials())?,
    })
}

/// Puts the current revocations and server leaf into force without a
/// restart: new CRLs, a new TLS config on every endpoint (new handshakes
/// only; live connections are the kick's business), and a `CrlUpdate` to
/// every client in the room. Call it after anything that revokes a
/// certificate or replaces the server leaf, never while holding the store
/// lock.
pub async fn reload_tls(server: &Arc<Server>) -> Result<()> {
    let crl = {
        let mut store = server.store.lock().await;
        let ca = server.ca.read().await;
        let crls = build_crls(&mut store, &ca)?;
        if !server.endpoints.is_empty() {
            let quic = server_config(&server.config, &ca, &crls.handshake)?;
            for endpoint in &server.endpoints {
                endpoint.set_server_config(Some(quic.clone()));
            }
        }
        crls.clients
    };
    let crl_der = crl.to_vec();
    tracing::info!(bytes = crl_der.len(), "CRL and TLS config reloaded");
    *server.crl.write().unwrap_or_else(|e| e.into_inner()) = crl_der.clone();
    server
        .room
        .broadcast(&icu_proto::ControlMsg::CrlUpdate { crl_der });
    Ok(())
}

/// [`reload_tls`] for callers that cannot do anything about a failure but
/// log it: the store change it follows is already written, and the
/// fingerprint check still refuses a revoked certificate after the
/// handshake.
pub async fn reload_tls_logged(server: &Arc<Server>) {
    if let Err(e) = reload_tls(server).await {
        tracing::warn!(error = %e, "cannot reload the CRL");
    }
}

/// Binds one endpoint.
///
/// IPv6 addresses are bound `IPV6_V6ONLY`, so that the spec's default
/// `listen = ["0.0.0.0:4711", "[::]:4711"]` does not collide on Linux, where
/// a v6 socket would otherwise also claim the v4 port.
pub fn bind(addr: SocketAddr, config: quinn::ServerConfig) -> Result<Endpoint> {
    let domain = match addr {
        SocketAddr::V4(_) => socket2::Domain::IPV4,
        SocketAddr::V6(_) => socket2::Domain::IPV6,
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))
        .context(format!("cannot create a socket for {addr}"))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true).context("cannot set IPV6_V6ONLY")?;
    }
    socket
        .bind(&addr.into())
        .context(format!("cannot bind {addr}"))?;
    let runtime =
        quinn::default_runtime().ok_or_else(|| Error::msg("no async runtime found for quinn"))?;
    Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(config),
        socket.into(),
        runtime,
    )
    .context(format!("cannot create an endpoint on {addr}"))
}

/// Accepts connections and dispatches by ALPN (§6.2).
pub async fn accept_loop(server: Arc<Server>, endpoint: Endpoint) {
    loop {
        let incoming = tokio::select! {
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => incoming,
                None => return,
            },
            _ = server.shutdown_requested() => return,
        };
        if let Some(handshake) = admit(&server, incoming) {
            let server = server.clone();
            tokio::spawn(async move {
                let remote = handshake.remote;
                if let Err(e) = dispatch(server, handshake).await {
                    tracing::debug!(%remote, error = %e, "connection ended");
                }
            });
        }
    }
}

/// Decides about one connection attempt (§6.2): refused, sent through a
/// Retry, or booked as a handshake in flight. Only handshakes count against
/// the ceiling — established connections are the business of
/// `limits.max_connections` — so whatever stalls in the handshake cannot
/// lock out a reconnect or a login.
fn admit(server: &Arc<Server>, incoming: quinn::Incoming) -> Option<Handshake> {
    let remote = incoming.remote_address();
    let handshakes = server.handshakes.load(Ordering::Relaxed);
    // Hard ceiling over all endpoints: whatever gets past the limits below
    // cannot pile up handshake state.
    if handshakes >= server.config.limits.max_handshakes as usize {
        tracing::debug!(%remote, handshakes, "handshake limit, refusing");
        incoming.refuse();
        return None;
    }
    // Address validation under load comes first: a Retry costs no budget,
    // so spoofed Initials cannot spend somebody else's. Without a validated
    // address a Retry is always allowed.
    let validated = incoming.remote_address_validated();
    if !validated && handshakes >= pl::HANDSHAKES_IN_FLIGHT_RETRY {
        retry(incoming);
        return None;
    }
    let ip = remote.ip();
    let mut limits = lock(&server.ip_limits);
    // A validated source holds a few handshakes at a time, however fast its
    // rate bucket refills. An unvalidated one is not counted: its address
    // may be spoofed, and unvalidated handshakes are bounded by the Retry
    // threshold above anyway.
    if validated && !limits.handshake_slot_free(ip) {
        tracing::debug!(%remote, "handshakes in flight from this source, refusing");
        drop(limits);
        incoming.refuse();
        return None;
    }
    match limits.allow_handshake(ip) {
        Some(true) => {}
        // The table is full of addresses nobody validated: a validated one
        // passes on the server wide budget alone, anything else has to prove
        // its address.
        None if validated => {
            if !limits.allow_untracked_handshake() {
                tracing::debug!(%remote, "global handshake rate limit, refusing");
                drop(limits);
                incoming.refuse();
                return None;
            }
        }
        None => {
            drop(limits);
            retry(incoming);
            return None;
        }
        Some(false) => {
            tracing::debug!(%remote, "handshake rate limit, refusing");
            drop(limits);
            incoming.refuse();
            return None;
        }
    }
    if validated {
        limits.start_handshake(ip);
    }
    drop(limits);
    server.handshakes.fetch_add(1, Ordering::Relaxed);
    Some(Handshake {
        server: server.clone(),
        incoming: Some(incoming),
        remote,
        booked: validated,
    })
}

/// Answers with a Retry; refuses when that is not allowed any more.
fn retry(incoming: quinn::Incoming) {
    if let Err(e) = incoming.retry() {
        e.into_incoming().refuse();
    }
}

/// One handshake in flight, counted in [`Server::handshakes`] and, when its
/// address was validated, against its source in [`crate::limits::IpLimits`]
/// until it completes, fails or times out.
struct Handshake {
    server: Arc<Server>,
    incoming: Option<quinn::Incoming>,
    remote: SocketAddr,
    booked: bool,
}

impl Drop for Handshake {
    fn drop(&mut self) {
        self.server.handshakes.fetch_sub(1, Ordering::Relaxed);
        if self.booked {
            lock(&self.server.ip_limits).end_handshake(self.remote.ip());
        }
    }
}

/// Counts one established connection in [`Server::open_connections`] for as
/// long as its task lives.
struct OpenConnection(Arc<Server>);

impl OpenConnection {
    /// A slot for one more established connection, `None` at
    /// `limits.max_connections`.
    fn claim(server: &Arc<Server>) -> Option<OpenConnection> {
        let max = server.config.limits.max_connections as usize;
        server
            .open_connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| OpenConnection(server.clone()))
    }
}

impl Drop for OpenConnection {
    fn drop(&mut self) {
        self.0.open_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn dispatch(server: Arc<Server>, mut handshake: Handshake) -> Result<()> {
    let remote = handshake.remote;
    let Some(incoming) = handshake.incoming.take() else {
        return Ok(());
    };
    let mut connecting = incoming.accept()?;

    // One deadline for the whole handshake. Otherwise only the idle timeout
    // (60 s) ends one that stalls, and a peer that answers just enough to
    // keep it alive holds server state for that long. Dropping `connecting`
    // closes the connection.
    let deadline = tokio::time::Instant::now() + server.config.timing.handshake_timeout();
    let too_slow = || Error::msg("handshake timed out");

    // Spike 2b: `handshake_data()` resolves before the handshake completes and
    // takes `&mut self`; the result needs a downcast (A2).
    let alpn = tokio::time::timeout_at(deadline, connecting.handshake_data())
        .await
        .map_err(|_| too_slow())?
        .ok()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|data| data.protocol.clone());
    let conn = tokio::time::timeout_at(deadline, connecting)
        .await
        .map_err(|_| too_slow())??;

    // From here on this is an established connection, not a handshake.
    drop(handshake);
    let Some(_open) = OpenConnection::claim(&server) else {
        tracing::debug!(%remote, "connection limit, closing");
        crate::wire::close(&conn, icu_proto::error::SERVER_FULL);
        return Ok(());
    };

    match alpn.as_deref() {
        Some(p) if p == pl::ALPN_MAIN => crate::session::handle(server, conn).await,
        other => {
            // rustls refuses an unknown ALPN during the handshake, so this is
            // only reachable when no ALPN was offered at all.
            tracing::debug!(%remote, alpn = ?other, "no usable ALPN");
            crate::wire::close(&conn, icu_proto::error::PROTOCOL_VIOLATION);
            Ok(())
        }
    }
}
