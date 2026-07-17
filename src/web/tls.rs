//! TLS setup for the web frontend (HTTPS/WSS).
//!
//! Three cert modes, resolved by [`resolve`] from the `[web]` config (in
//! precedence order):
//! 1. **ACME / Let's Encrypt** — when `acme_domains` is set; fetches a trusted
//!    cert via TLS-ALPN-01 (needs public DNS + port 443).
//! 2. **Operator-provided** — when `tls_cert` and `tls_key` point at PEM files.
//! 3. **Self-signed (default)** — otherwise a persistent self-signed cert is
//!    generated so TLS works out of the box.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;

use crate::config::Web;

/// A future that drives background TLS work (the ACME event loop).
type Driver = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The resolved TLS acceptor for [`crate::web::serve_tls`].
pub enum TlsSetup {
    /// A ready rustls config (self-signed or operator-provided cert).
    Rustls(RustlsConfig),
    /// An ACME acceptor plus the state-machine driver that must be spawned.
    Acme {
        acceptor: rustls_acme::axum::AxumAcceptor,
        driver: Driver,
    },
}

/// Resolve the TLS setup for the web frontend from its config.
pub async fn resolve(web: &Web) -> Result<TlsSetup> {
    if !web.acme_domains.is_empty() {
        return resolve_acme(web);
    }
    match (web.tls_cert.trim(), web.tls_key.trim()) {
        ("", "") => {
            // Self-signed default: generate once, persist, reuse.
            let cfg = self_signed_config("web-cert.pem", "web-key.pem", web).await?;
            Ok(TlsSetup::Rustls(cfg))
        }
        (cert, key) if !cert.is_empty() && !key.is_empty() => {
            let cfg = RustlsConfig::from_pem_file(cert, key)
                .await
                .with_context(|| format!("loading [web] tls_cert {cert} / tls_key {key}"))?;
            Ok(TlsSetup::Rustls(cfg))
        }
        _ => anyhow::bail!(
            "set both [web] tls_cert and tls_key, or neither (to auto-generate a self-signed cert)"
        ),
    }
}

/// Load the self-signed cert at `cert_path`/`key_path`, generating and
/// persisting a new one if either file is missing.
async fn self_signed_config(cert_path: &str, key_path: &str, web: &Web) -> Result<RustlsConfig> {
    let have = std::path::Path::new(cert_path).exists() && std::path::Path::new(key_path).exists();
    if !have {
        let (cert_pem, key_pem) = self_signed_pems(self_signed_sans(web))?;
        std::fs::write(cert_path, &cert_pem).with_context(|| format!("writing {cert_path}"))?;
        std::fs::write(key_path, &key_pem).with_context(|| format!("writing {key_path}"))?;
        tracing::info!("generated a self-signed web TLS cert at {cert_path} / {key_path}");
    }
    RustlsConfig::from_pem_file(cert_path, key_path)
        .await
        .with_context(|| format!("loading self-signed cert {cert_path} / {key_path}"))
}

/// Generate a self-signed cert + key as PEM strings for the given SAN names.
pub fn self_signed_pems(sans: Vec<String>) -> Result<(String, String)> {
    let ck = rcgen::generate_simple_self_signed(sans).context("generating self-signed cert")?;
    Ok((ck.cert.pem(), ck.signing_key.serialize_pem()))
}

/// Subject-alt-names for the generated cert: always `localhost`, plus the
/// configured host when it's a concrete name (not a wildcard bind address).
fn self_signed_sans(web: &Web) -> Vec<String> {
    let mut sans = vec!["localhost".to_string()];
    let host = web.host.trim();
    if !host.is_empty()
        && !matches!(host, "0.0.0.0" | "::" | "[::]")
        && !sans.iter().any(|s| s == host)
    {
        sans.push(host.to_string());
    }
    sans
}

/// Build the ACME (Let's Encrypt) acceptor and its background driver.
fn resolve_acme(web: &Web) -> Result<TlsSetup> {
    use futures_util::StreamExt;
    use rustls_acme::{AcmeConfig, caches::DirCache};

    let contacts: Vec<String> = if web.acme_email.trim().is_empty() {
        Vec::new()
    } else {
        vec![format!("mailto:{}", web.acme_email.trim())]
    };
    let mut state = AcmeConfig::new(web.acme_domains.clone())
        .contact(contacts)
        .cache(DirCache::new(web.acme_cache.clone()))
        .directory_lets_encrypt(!web.acme_staging)
        .state();
    let acceptor = state.axum_acceptor(state.default_rustls_config());
    let driver: Driver = Box::pin(async move {
        loop {
            match state.next().await {
                Some(Ok(ok)) => tracing::info!("acme: {ok:?}"),
                Some(Err(e)) => tracing::error!("acme error: {e}"),
                None => break,
            }
        }
    });
    Ok(TlsSetup::Acme { acceptor, driver })
}

/// Fetch `/healthz` over TLS from a loopback address, trusting any certificate
/// (the startup self-check only needs to confirm *bbs-rs* is the responder, not
/// validate the cert). Used when the web frontend serves HTTPS.
pub async fn probe_health_tls(host: &str, port: u16) -> Result<String> {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
    let tcp = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await??;
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())?;
    let mut tls = connector.connect(server_name, tcp).await?;
    let req = format!("GET /healthz HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tls.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), tls.read_to_end(&mut buf)).await??;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// A rustls certificate verifier that accepts everything — only for the
/// loopback self-check, never for real client connections.
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    #[tokio::test]
    async fn self_signed_generates_then_reuses() {
        provider();
        let dir = std::env::temp_dir().join(format!("bbs_tls_ss_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cert = dir.join("c.pem");
        let key = dir.join("k.pem");
        let (cp, kp) = (cert.to_str().unwrap(), key.to_str().unwrap());

        self_signed_config(cp, kp, &Web::default()).await.unwrap();
        assert!(cert.exists() && key.exists(), "cert/key are written");
        let first = std::fs::read(&cert).unwrap();

        // A second call reuses the existing cert (no regeneration).
        self_signed_config(cp, kp, &Web::default()).await.unwrap();
        assert_eq!(first, std::fs::read(&cert).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resolve_selects_acme_when_domains_set() {
        provider();
        let web = Web {
            acme_domains: vec!["bbs.example.com".into()],
            ..Web::default()
        };
        assert!(matches!(
            resolve(&web).await.unwrap(),
            TlsSetup::Acme { .. }
        ));
    }

    #[tokio::test]
    async fn resolve_uses_provided_cert_files() {
        provider();
        let dir = std::env::temp_dir().join(format!("bbs_tls_byo_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (c, k) = self_signed_pems(vec!["localhost".into()]).unwrap();
        let cp = dir.join("c.pem");
        let kp = dir.join("k.pem");
        std::fs::write(&cp, c).unwrap();
        std::fs::write(&kp, k).unwrap();
        let web = Web {
            tls_cert: cp.to_string_lossy().into_owned(),
            tls_key: kp.to_string_lossy().into_owned(),
            ..Web::default()
        };
        assert!(matches!(resolve(&web).await.unwrap(), TlsSetup::Rustls(_)));

        // Only one of the pair set → an explicit error.
        let half = Web {
            tls_cert: cp.to_string_lossy().into_owned(),
            ..Web::default()
        };
        assert!(resolve(&half).await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
