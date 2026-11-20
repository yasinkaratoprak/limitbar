//! TLS for the Antigravity language server, which listens on loopback with a self-signed
//! certificate. The certificate chain cannot be checked against a CA, so trust is limited
//! to the loopback address itself: any other server name is rejected, and the handshake
//! signature is still verified so the connection is genuinely encrypted to that peer.

use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
struct LoopbackOnly(Arc<CryptoProvider>);

pub fn is_loopback(name: &ServerName<'_>) -> bool {
    matches!(name, ServerName::IpAddress(ip) if std::net::IpAddr::from(*ip).is_loopback())
}

impl ServerCertVerifier for LoopbackOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if is_loopback(server_name) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "self-signed certificates are only trusted on loopback".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// An HTTPS agent for `https://127.0.0.1:<port>` that accepts the server's self-signed certificate.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(LoopbackOnly(provider)))
        .with_no_client_auth();
    ureq::AgentBuilder::new()
        .tls_config(Arc::new(config))
        .timeout(timeout)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_names_are_trusted() {
        assert!(is_loopback(&ServerName::try_from("127.0.0.1").unwrap()));
        assert!(is_loopback(&ServerName::try_from("::1").unwrap()));
        assert!(!is_loopback(&ServerName::try_from("localhost").unwrap()));
        assert!(!is_loopback(&ServerName::try_from("10.0.0.1").unwrap()));
        assert!(!is_loopback(&ServerName::try_from("example.com").unwrap()));
    }

    /// Run against a throwaway self-signed server:
    /// `openssl s_server -accept 127.0.0.1:PORT -www -cert c.pem -key k.pem`, then
    /// `LIMITBAR_TLS_TEST_PORT=PORT cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn connects_to_self_signed_loopback_server() {
        let port: u16 = std::env::var("LIMITBAR_TLS_TEST_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let response = agent(Duration::from_secs(5))
            .get(&format!("https://127.0.0.1:{port}/"))
            .call()
            .expect("TLS handshake with self-signed loopback server");
        assert_eq!(response.status(), 200);
    }
}
