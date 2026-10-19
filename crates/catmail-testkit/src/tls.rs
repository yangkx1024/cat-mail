use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// A self-signed certificate for `localhost` / `127.0.0.1`, generated per test.
#[derive(Clone)]
pub struct TestCert {
    cert: CertificateDer<'static>,
    server_config: Arc<rustls::ServerConfig>,
}

impl TestCert {
    pub fn generate() -> Self {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .expect("generate test certificate");
        let cert = CertificateDer::from(key.cert.der().to_vec());
        let private_key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.signing_key.serialize_der()));
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], private_key)
        .expect("server certificate");
        Self {
            cert,
            server_config: Arc::new(server_config),
        }
    }

    /// The DER certificate a client must trust to talk to the server.
    pub fn cert_der(&self) -> &[u8] {
        &self.cert
    }

    pub(crate) fn acceptor(&self) -> tokio_rustls::TlsAcceptor {
        tokio_rustls::TlsAcceptor::from(self.server_config.clone())
    }
}
