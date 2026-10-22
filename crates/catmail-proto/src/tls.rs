use std::sync::Arc;

use rustls::ClientConfig;
use rustls::pki_types::CertificateDer;
use rustls_platform_verifier::Verifier;

/// How server certificates are verified. Cheap to clone.
#[derive(Clone)]
pub struct TlsConfig(Arc<ClientConfig>);

impl TlsConfig {
    /// The operating system's trust store and verifier, so enterprise CAs installed on the
    /// machine are honoured the same way as in the user's browser.
    pub fn platform() -> Result<Self, rustls::Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Verifier::new(provider.clone())?;
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous() // Not dangerous: installs the platform verifier, not a no-op.
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        Ok(Self(Arc::new(config)))
    }

    /// Trusts exactly `roots` (DER-encoded certificates) and nothing from the platform,
    /// e.g. a self-signed test server.
    pub fn with_roots_only<'a>(
        roots: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Self, rustls::Error> {
        let mut store = rustls::RootCertStore::empty();
        for root in roots {
            store.add(CertificateDer::from(root.to_vec()))?;
        }
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(store)
                .with_no_client_auth();
        Ok(Self(Arc::new(config)))
    }

    pub(crate) fn connector(&self) -> tokio_rustls::TlsConnector {
        tokio_rustls::TlsConnector::from(self.0.clone())
    }

    /// For the HTTP client, which takes the configuration by value.
    pub(crate) fn client_config(&self) -> ClientConfig {
        (*self.0).clone()
    }
}

impl std::fmt::Debug for TlsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConfig").finish_non_exhaustive()
    }
}
