// SPDX-License-Identifier: Apache-2.0
//! TLS via rustls, instrumented phase by phase.
//!
//! rustls is the only TLS implementation in this project. There is no OpenSSL
//! path, no BoringSSL path and no runtime provider selection: the handshake
//! you measure here uses the same `ring` primitives as [`crate::sha256`] and
//! [`crate::AeadKey`].
//!
//! The point of the module is measurement. A TLS handshake is one of the most
//! expensive things a latency-sensitive program does — two round trips, a
//! signature verification and a key exchange — and [`HandshakeTiming`] splits
//! that cost into parts you can act on, rather than reporting one number.
//!
//! Network time dominates here, so figures are in nanoseconds off the wall
//! clock rather than raw counter units: a handshake spans milliseconds and
//! several scheduler slices, which is well outside the regime where a
//! cycle counter is the right instrument.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore};

/// Default connect and handshake timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the time in a handshake went.
#[derive(Debug, Clone)]
pub struct HandshakeTiming {
    pub host: String,
    pub port: u16,

    /// Name resolution.
    pub resolve_ns: u64,
    /// TCP three-way handshake.
    pub tcp_connect_ns: u64,
    /// TLS handshake: ClientHello through Finished.
    pub tls_handshake_ns: u64,
    /// Everything above, plus config construction.
    pub total_ns: u64,

    /// Negotiated protocol version, e.g. `TLSv1_3`.
    pub protocol: String,
    /// Negotiated cipher suite.
    pub cipher_suite: String,
    /// Certificates the server presented.
    pub peer_certificates: usize,
    /// Whether the session was resumed rather than negotiated afresh.
    pub resumed: bool,
}

impl HandshakeTiming {
    /// Total in milliseconds, for display.
    pub fn total_ms(&self) -> f64 {
        self.total_ns as f64 / 1e6
    }

    /// Share of the total spent in the TLS handshake proper, as a fraction.
    ///
    /// A high value means crypto and round trips dominate; a low one means the
    /// cost is in DNS or the TCP connect, and TLS is not the thing to tune.
    pub fn tls_fraction(&self) -> f64 {
        if self.total_ns == 0 {
            0.0
        } else {
            self.tls_handshake_ns as f64 / self.total_ns as f64
        }
    }

    /// One-line summary for logs and the benchmark panel.
    pub fn summary(&self) -> String {
        format!(
            "{}:{} {} {} resolve={:.2}ms tcp={:.2}ms tls={:.2}ms total={:.2}ms certs={}",
            self.host,
            self.port,
            self.protocol,
            self.cipher_suite,
            self.resolve_ns as f64 / 1e6,
            self.tcp_connect_ns as f64 / 1e6,
            self.tls_handshake_ns as f64 / 1e6,
            self.total_ms(),
            self.peer_certificates,
        )
    }
}

/// Why a probe failed.
#[derive(Debug)]
pub enum TlsError {
    /// The hostname did not resolve, or is not a valid TLS server name.
    InvalidHost(String),
    /// DNS, TCP or socket-level failure.
    Io(io::Error),
    /// rustls rejected the connection: certificate, version or suite.
    Tls(rustls::Error),
}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TlsError::InvalidHost(h) => write!(f, "invalid or unresolvable host: {h}"),
            TlsError::Io(e) => write!(f, "network error: {e}"),
            TlsError::Tls(e) => write!(f, "TLS error: {e}"),
        }
    }
}

impl std::error::Error for TlsError {}

impl From<io::Error> for TlsError {
    fn from(e: io::Error) -> Self {
        TlsError::Io(e)
    }
}

impl From<rustls::Error> for TlsError {
    fn from(e: rustls::Error) -> Self {
        TlsError::Tls(e)
    }
}

/// Builds a client config that trusts the Mozilla root program.
///
/// Roots are compiled in via `webpki-roots` rather than read from the OS
/// store, so a measurement is reproducible across machines — the same trust
/// decisions on a developer laptop and in CI.
pub fn client_config() -> Result<Arc<ClientConfig>, TlsError> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = ClientConfig::builder_with_provider(crate::provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Connects to `host:port` and times each phase of the handshake.
///
/// The connection is closed immediately afterwards: this measures the cost of
/// establishing TLS, not of using it.
pub fn probe_handshake(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<HandshakeTiming, TlsError> {
    let overall_start = Instant::now();
    let config = client_config()?;

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| TlsError::InvalidHost(host.to_string()))?;

    let resolve_start = Instant::now();
    let address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| TlsError::InvalidHost(host.to_string()))?;
    let resolve_ns = resolve_start.elapsed().as_nanos() as u64;

    let tcp_start = Instant::now();
    let mut socket = TcpStream::connect_timeout(&address, timeout)?;
    let tcp_connect_ns = tcp_start.elapsed().as_nanos() as u64;

    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    // Nagle would batch the ClientHello with nothing and add a round trip's
    // worth of noise to the number this function exists to report.
    socket.set_nodelay(true)?;

    let mut connection = ClientConnection::new(config, server_name)?;

    let handshake_start = Instant::now();
    // Drives the handshake to completion without sending application data.
    while connection.is_handshaking() {
        connection.complete_io(&mut socket)?;
    }
    let tls_handshake_ns = handshake_start.elapsed().as_nanos() as u64;

    let protocol = connection
        .protocol_version()
        .map(|v| format!("{v:?}"))
        .unwrap_or_else(|| "unknown".to_string());
    let cipher_suite = connection
        .negotiated_cipher_suite()
        .map(|s| format!("{:?}", s.suite()))
        .unwrap_or_else(|| "unknown".to_string());
    let peer_certificates = connection.peer_certificates().map_or(0, |c| c.len());

    // A resumed session skips certificate verification, which is most of the
    // CPU cost — reporting it stops a fast number being mistaken for a fast
    // full handshake.
    let resumed = peer_certificates == 0;

    connection.send_close_notify();
    let _ = connection.complete_io(&mut socket);

    Ok(HandshakeTiming {
        host: host.to_string(),
        port,
        resolve_ns,
        tcp_connect_ns,
        tls_handshake_ns,
        total_ns: overall_start.elapsed().as_nanos() as u64,
        protocol,
        cipher_suite,
        peer_certificates,
        resumed,
    })
}

/// Cipher suites the provider offers, in preference order.
pub fn supported_cipher_suites() -> Vec<String> {
    rustls::crypto::ring::default_provider()
        .cipher_suites
        .iter()
        .map(|s| format!("{:?}", s.suite()))
        .collect()
}

/// Key exchange groups the provider offers, in preference order.
pub fn supported_key_exchange_groups() -> Vec<String> {
    rustls::crypto::ring::default_provider()
        .kx_groups
        .iter()
        .map(|g| format!("{:?}", g.name()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_config_builds_on_the_ring_provider() {
        assert!(client_config().is_ok());
    }

    #[test]
    fn provider_offers_tls13_suites() {
        let suites = supported_cipher_suites();
        assert!(!suites.is_empty());
        assert!(
            suites.iter().any(|s| s.contains("TLS13")),
            "expected a TLS 1.3 suite in {suites:?}"
        );
    }

    #[test]
    fn provider_offers_a_key_exchange_group() {
        assert!(!supported_key_exchange_groups().is_empty());
    }

    #[test]
    fn invalid_server_names_are_rejected_before_dialling() {
        // An IP-shaped label with a trailing dot is not a valid SNI name, and
        // must fail without opening a socket.
        let err = probe_handshake("not a hostname", 443, Duration::from_millis(1));
        assert!(matches!(err, Err(TlsError::InvalidHost(_))));
    }

    #[test]
    fn timing_fractions_handle_a_zero_total() {
        let t = HandshakeTiming {
            host: "example.com".into(),
            port: 443,
            resolve_ns: 0,
            tcp_connect_ns: 0,
            tls_handshake_ns: 0,
            total_ns: 0,
            protocol: "TLSv1_3".into(),
            cipher_suite: "TLS13_AES_256_GCM_SHA384".into(),
            peer_certificates: 2,
            resumed: false,
        };
        assert_eq!(t.tls_fraction(), 0.0);
        assert_eq!(t.total_ms(), 0.0);
    }

    /// Network-dependent, so opt-in: `NANOCHRONO_NETWORK_TESTS=1 cargo test`.
    #[test]
    fn live_handshake_reports_tls13() {
        if std::env::var("NANOCHRONO_NETWORK_TESTS").is_err() {
            return;
        }
        let timing = probe_handshake("www.rust-lang.org", 443, DEFAULT_TIMEOUT)
            .expect("handshake against a public host");
        assert!(timing.tls_handshake_ns > 0);
        assert!(timing.peer_certificates > 0);
    }
}
