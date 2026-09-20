//! QUIC endpoint: TLS configuration, binding and the accept loop
//! (`Server/IMPLEMENTATION.md` §6.1.5–§6.2).

use std::{net::SocketAddr, sync::Arc, time::Duration};

use icu_proto::limits as pl;
use quinn::{crypto::rustls::QuicServerConfig, Endpoint, VarInt};
use rustls::RootCertStore;

use crate::{
    ca::Ca,
    config::Config,
    error::{Context, Error, Result},
    Server,
};

/// Builds the rustls and quinn server configuration.
///
/// The order below is load bearing: `builder_with_provider` yields
/// `WantsVersions`, so `with_protocol_versions` has to come **before** the
/// client certificate verifier (A8). The verifier uses
/// `allow_unauthenticated()`, because the enrollment ALPN must work without a
/// client certificate; the requirement is enforced per ALPN in `session.rs`
/// (`PROTOCOL.md` §1, Spike 2b).
pub fn server_config(config: &Config, ca: &Ca) -> Result<quinn::ServerConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(ca.ca_der().clone())
        .context("cannot add the ICU CA to the root store")?;

    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .allow_unauthenticated()
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

    tls.alpn_protocols = vec![pl::ALPN_MAIN.to_vec(), pl::ALPN_ENROLL.to_vec()];
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
        let allowed = server
            .ip_limits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allow_handshake(remote.ip());
        if !allowed {
            tracing::debug!(%remote, "handshake rate limit, refusing");
            incoming.refuse();
            continue;
        }
        // Address validation under load (§6.2).
        let incoming = if endpoint.open_connections() >= pl::HANDSHAKES_IN_FLIGHT_RETRY
            && incoming.may_retry()
        {
            match incoming.retry() {
                Ok(()) => continue,
                Err(e) => e.into_incoming(),
            }
        } else {
            incoming
        };

        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = dispatch(server, incoming).await {
                tracing::debug!(%remote, error = %e, "connection ended");
            }
        });
    }
}

async fn dispatch(server: Arc<Server>, incoming: quinn::Incoming) -> Result<()> {
    let remote = incoming.remote_address();
    let mut connecting = incoming.accept()?;

    // Spike 2b: `handshake_data()` resolves before the handshake completes and
    // takes `&mut self`; the result needs a downcast (A2).
    let alpn = connecting
        .handshake_data()
        .await
        .ok()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|data| data.protocol.clone());

    match alpn.as_deref() {
        Some(p) if p == pl::ALPN_ENROLL => {
            let conn = connecting.await?;
            crate::enroll::handle(server, conn, remote).await
        }
        Some(p) if p == pl::ALPN_MAIN => {
            let conn = connecting.await?;
            crate::session::handle(server, conn).await
        }
        other => {
            // rustls refuses an unknown ALPN during the handshake, so this is
            // only reachable when no ALPN was offered at all.
            tracing::debug!(%remote, alpn = ?other, "no usable ALPN");
            let conn = connecting.await?;
            conn.close(
                VarInt::from_u32(icu_proto::error::PROTOCOL_VIOLATION),
                b"alpn",
            );
            Ok(())
        }
    }
}
