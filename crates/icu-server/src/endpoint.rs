//! QUIC endpoint: TLS configuration, binding and the accept loop
//! (`Server/IMPLEMENTATION.md` §6.1.5–§6.2).

use std::{
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use icu_proto::limits as pl;
use quinn::{crypto::rustls::QuicServerConfig, Endpoint, VarInt};
use rustls::{pki_types::CertificateRevocationListDer, RootCertStore};

use crate::{
    ca::Ca,
    config::Config,
    error::{Context, Error, Result},
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

    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .context("TLS 1.3 is not available in this provider")?
    .with_client_cert_verifier(verifier)
    .with_single_cert(ca.server_chain(), ca.server_key()?)
    .context("cannot install the server certificate")?;

    tls.alpn_protocols = vec![pl::ALPN_MAIN.to_vec()];
    // 0-RTT off (`PROTOCOL.md` §1).
    tls.max_early_data_size = 0;
    tls.send_half_rtt_data = false;

    let mut quic = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    quic.migration(true);

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
    transport.max_concurrent_bidi_streams(VarInt::from_u32(2));
    quic.transport_config(Arc::new(transport));

    Ok(quic)
}

/// Signs a fresh CRL from the store under the next CRL number.
pub fn build_crl(store: &mut Store, ca: &Ca) -> Result<CertificateRevocationListDer<'static>> {
    let number = store.next_crl_number()?;
    ca.crl(number, &store.crl_serials())
}

/// Puts the current revocations and server leaf into force without a
/// restart: a new CRL, a new TLS config on every endpoint (new handshakes
/// only; live connections are the kick's business), and a `CrlUpdate` to
/// every client in the room. Call it after anything that revokes a
/// certificate or replaces the server leaf, never while holding the store
/// lock.
pub async fn reload_tls(server: &Arc<Server>) -> Result<()> {
    let crl = {
        let mut store = server.store.lock().await;
        let ca = server.ca.read().await;
        let crl = build_crl(&mut store, &ca)?;
        if !server.endpoints.is_empty() {
            let quic = server_config(&server.config, &ca, &crl)?;
            for endpoint in &server.endpoints {
                endpoint.set_server_config(Some(quic.clone()));
            }
        }
        crl
    };
    *server.crl.write().unwrap_or_else(|e| e.into_inner()) = crl.to_vec();
    server.room.broadcast(&icu_proto::ControlMsg::CrlUpdate {
        crl_der: crl.to_vec(),
    });
    tracing::info!(bytes = crl.len(), "CRL and TLS config reloaded");
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

        let remote = incoming.remote_address();
        // Hard ceiling over all endpoints, handshaking or established (§6.2):
        // whatever gets past the rate limits cannot pile up connection state.
        let open = server.open_connections.load(Ordering::Relaxed);
        if open >= server.config.limits.max_connections as usize {
            tracing::debug!(%remote, open, "connection limit, refusing");
            incoming.refuse();
            continue;
        }
        // Address validation under load (§6.2) comes first: a Retry costs
        // no budget, so spoofed Initials cannot spend somebody else's.
        let incoming = if open >= pl::HANDSHAKES_IN_FLIGHT_RETRY && incoming.may_retry() {
            match incoming.retry() {
                Ok(()) => continue,
                Err(e) => e.into_incoming(),
            }
        } else {
            incoming
        };
        let allowed = server
            .ip_limits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allow_handshake(remote.ip());
        match allowed {
            Some(true) => {}
            // The table is full of addresses nobody validated: a validated
            // one passes on the server wide budget alone, anything else has
            // to prove its address.
            None if incoming.remote_address_validated() => {
                let ok = server
                    .ip_limits
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .allow_untracked_handshake();
                if !ok {
                    tracing::debug!(%remote, "global handshake rate limit, refusing");
                    incoming.refuse();
                    continue;
                }
            }
            None if incoming.may_retry() => {
                if let Err(e) = incoming.retry() {
                    e.into_incoming().refuse();
                }
                continue;
            }
            Some(false) | None => {
                tracing::debug!(%remote, "handshake rate limit, refusing");
                incoming.refuse();
                continue;
            }
        }

        let server = server.clone();
        let guard = OpenConnection::new(&server);
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(e) = dispatch(server, incoming).await {
                tracing::debug!(%remote, error = %e, "connection ended");
            }
        });
    }
}

/// Counts one connection task in [`Server::open_connections`] for as long as
/// it lives, handshake included.
struct OpenConnection(Arc<Server>);

impl OpenConnection {
    fn new(server: &Arc<Server>) -> OpenConnection {
        server.open_connections.fetch_add(1, Ordering::Relaxed);
        OpenConnection(server.clone())
    }
}

impl Drop for OpenConnection {
    fn drop(&mut self) {
        self.0.open_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn dispatch(server: Arc<Server>, incoming: quinn::Incoming) -> Result<()> {
    let remote = incoming.remote_address();
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

    match alpn.as_deref() {
        Some(p) if p == pl::ALPN_MAIN => crate::session::handle(server, conn).await,
        other => {
            // rustls refuses an unknown ALPN during the handshake, so this is
            // only reachable when no ALPN was offered at all.
            tracing::debug!(%remote, alpn = ?other, "no usable ALPN");
            conn.close(
                VarInt::from_u32(icu_proto::error::PROTOCOL_VIOLATION),
                b"alpn",
            );
            Ok(())
        }
    }
}
