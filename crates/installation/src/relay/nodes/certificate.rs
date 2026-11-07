//! The manager's node certificate. Its key ends both node paths: DTLS on a
//! direct connection and TLS inside a Beacon channel. A node pins it from its
//! install command, so Beacon can neither read nor impersonate the manager.
use crate::error::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::SystemTime,
};
use tokio::sync::Mutex;

const FILE: &str = "node-certificate.json";

#[derive(Serialize, Deserialize)]
struct Stored {
    certificate: String,
    key: String,
}

pub(crate) struct NodeCertificate {
    certificate: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    pub pin: String,
}

// Serializes creation, so concurrent first uses never return different pins.
static LOADED: LazyLock<Mutex<HashMap<PathBuf, Arc<NodeCertificate>>>> =
    LazyLock::new(Mutex::default);

impl NodeCertificate {
    /// Loads the certificate under `directory`, creating it on first use.
    pub(crate) async fn load(directory: &Path) -> Result<Arc<Self>> {
        let mut loaded = LOADED.lock().await;
        if let Some(certificate) = loaded.get(directory) {
            return Ok(certificate.clone());
        }
        let path = directory.join(FILE);
        let stored = match super::super::read_private::<Stored>(&path).await? {
            Some(stored) => stored,
            None => {
                crate::skills::private_dir(directory).await?;
                let stored = generate()?;
                crate::native::atomic_write(&path, &serde_json::to_vec(&stored)?).await?;
                stored
            }
        };
        let decode = |value: &str| {
            STANDARD
                .decode(value)
                .map_err(|_| Error::bad("Invalid node certificate."))
        };
        let certificate = CertificateDer::from(decode(&stored.certificate)?);
        let certificate = Arc::new(Self {
            pin: cairn_protocol::nodes::certificate_pin(&certificate),
            certificate,
            key: PrivatePkcs8KeyDer::from(decode(&stored.key)?),
        });
        loaded.insert(directory.to_owned(), certificate.clone());
        Ok(certificate)
    }

    pub(crate) fn tls(&self) -> Result<tokio_rustls::TlsAcceptor> {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(Error::internal)?
        .with_no_client_auth()
        .with_single_cert(
            vec![self.certificate.clone()],
            PrivateKeyDer::Pkcs8(self.key.clone_key()),
        )
        .map_err(Error::internal)?;
        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
    }

    pub(crate) fn webrtc(&self) -> Result<rtc::peer_connection::certificate::RTCCertificate> {
        let provider = rtc::crypto::default_provider().map_err(Error::internal)?;
        rtc::peer_connection::certificate::RTCCertificate::from_pkcs8(
            provider.crypto(),
            rtc::crypto::SignatureScheme::EcdsaP256Sha256,
            vec![self.certificate.clone()],
            self.key.secret_pkcs8_der(),
            // The pin, not a validity period, is what the node trusts.
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(253_402_300_799),
        )
        .map_err(Error::internal)
    }
}

fn generate() -> Result<Stored> {
    let key =
        rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(Error::internal)?;
    let mut params =
        rcgen::CertificateParams::new(vec!["cairn-manager".to_owned()]).map_err(Error::internal)?;
    params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    params.not_after = rcgen::date_time_ymd(9999, 12, 31);
    let certificate = params.self_signed(&key).map_err(Error::internal)?;
    Ok(Stored {
        certificate: STANDARD.encode(certificate.der()),
        key: STANDARD.encode(key.serialize_der()),
    })
}
