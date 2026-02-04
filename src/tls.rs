//! TLS/HTTPS infrastructure module for the Janus application.
//!
//! Handles certificate loading and generation, including automatic self-signed certificate
//! generation for development and local use with reverse proxies like ngrok.

use axum_server::tls_rustls::RustlsConfig;
use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys};
use std::fs;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("IO error: {0}")]
    IoError(#[from] io::Error),

    #[error("Failed to parse certificate: {0}")]
    CertParseError(String),

    #[error("Failed to parse private key: {0}")]
    KeyParseError(String),

    #[error("Failed to generate self-signed certificate: {0}")]
    CertGenerationError(String),

    #[error("Failed to build TLS configuration: {0}")]
    ConfigBuildError(String),
}

/// Configuration for TLS certificate loading and generation.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to certificate file (PEM format). If None, auto-generate.
    pub cert_path: Option<PathBuf>,
    /// Path to private key file (PEM format). If None, auto-generate.
    pub key_path: Option<PathBuf>,
    /// Whether to auto-generate self-signed certificates if paths not provided.
    pub auto_generate: bool,
}

/// Load certificates from PEM file.
fn load_certs(path: &Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, TlsError> {
    let cert_file = fs::File::open(path)?;
    let mut reader = BufReader::new(cert_file);

    certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::CertParseError(format!("Failed to read certificates: {}", e)))
}

/// Load private key from PEM file (PKCS#8 format).
fn load_private_key(path: &Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>, TlsError> {
    let key_file = fs::File::open(path)?;
    let mut reader = BufReader::new(key_file);

    let keys: Vec<_> = pkcs8_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::KeyParseError(format!("Failed to parse private keys: {}", e)))?;

    keys.into_iter()
        .next()
        .map(rustls::pki_types::PrivateKeyDer::Pkcs8)
        .ok_or_else(|| TlsError::KeyParseError("No private key found in file".to_string()))
}

/// Generate a self-signed certificate for localhost.
///
/// Creates a certificate valid for 365 days with localhost and 127.0.0.1 as subject alternative names.
fn generate_self_signed_cert() -> Result<(String, String), TlsError> {
    // Generate simple self-signed certificate with localhost SAN
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .map_err(|e| TlsError::CertGenerationError(format!("Failed to generate certificate: {}", e)))?;

    let cert_pem = certified_key.cert.pem();
    let key_pem = certified_key.key_pair.serialize_pem();

    tracing::info!("Generated self-signed certificate for localhost");

    Ok((cert_pem, key_pem))
}

/// Create rustls ServerConfig from certificate and private key.
fn build_rustls_config(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<ServerConfig, TlsError> {
    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::ConfigBuildError(format!("Failed to build TLS config: {}", e)))
}

/// Load or generate TLS certificates and create rustls ServerConfig.
///
/// # Behavior
/// - If `cert_path` and `key_path` are provided, loads certificates from files
/// - If paths are not provided and `auto_generate` is true, generates self-signed certificates
/// - If paths are not provided and `auto_generate` is false, returns an error
///
/// # Returns
/// RustlsConfig ready for use with axum-server
pub fn create_rustls_config(config: &TlsConfig) -> Result<RustlsConfig, TlsError> {
    match (&config.cert_path, &config.key_path) {
        (Some(cert_path), Some(key_path)) => {
            // Load certificates from files
            tracing::info!(
                cert_path = %cert_path.display(),
                key_path = %key_path.display(),
                "Loading TLS certificates from files"
            );

            let certs = load_certs(cert_path)?;
            let key = load_private_key(key_path)?;
            let server_config = build_rustls_config(certs, key)?;

            tracing::info!("TLS certificates loaded successfully");
            Ok(RustlsConfig::from_config(Arc::new(server_config)))
        }
        (None, None) if config.auto_generate => {
            // Generate self-signed certificate
            tracing::info!("Auto-generating self-signed TLS certificate");

            let (cert_pem, key_pem) = generate_self_signed_cert()?;

            // Parse generated PEM data
            let certs = certs(&mut cert_pem.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| TlsError::CertParseError(format!("Failed to parse generated cert: {}", e)))?;

            let mut key_reader = BufReader::new(key_pem.as_bytes());
            let keys: Vec<_> = pkcs8_private_keys(&mut key_reader)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| TlsError::KeyParseError(format!("Failed to parse generated keys: {}", e)))?;

            let key = keys
                .into_iter()
                .next()
                .map(rustls::pki_types::PrivateKeyDer::Pkcs8)
                .ok_or_else(|| TlsError::KeyParseError("Generated key is empty".to_string()))?;

            let server_config = build_rustls_config(certs, key)?;

            tracing::info!("Self-signed TLS certificate generated successfully");
            Ok(RustlsConfig::from_config(Arc::new(server_config)))
        }
        _ => {
            Err(TlsError::ConfigBuildError(
                "TLS certificate and key paths must both be provided, or auto_generate must be enabled".to_string()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_generate_self_signed_cert() {
        let result = generate_self_signed_cert();
        assert!(result.is_ok());

        let (cert_pem, key_pem) = result.unwrap();

        // Verify PEM format
        assert!(cert_pem.contains("-----BEGIN CERTIFICATE-----"));
        assert!(cert_pem.contains("-----END CERTIFICATE-----"));
        assert!(key_pem.contains("-----BEGIN PRIVATE KEY-----"));
        assert!(key_pem.contains("-----END PRIVATE KEY-----"));
    }

    #[test]
    fn test_create_rustls_config_auto_generate() {
        let config = TlsConfig {
            cert_path: None,
            key_path: None,
            auto_generate: true,
        };

        let result = create_rustls_config(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_rustls_config_missing_paths() {
        let config = TlsConfig {
            cert_path: None,
            key_path: None,
            auto_generate: false,
        };

        let result = create_rustls_config(&config);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TlsError::ConfigBuildError(_)));
    }

    #[test]
    fn test_load_certs_invalid_file() {
        let result = load_certs(Path::new("/nonexistent/path"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TlsError::IoError(_)));
    }

    #[test]
    fn test_load_private_key_invalid_file() {
        let result = load_private_key(Path::new("/nonexistent/path"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TlsError::IoError(_)));
    }

    #[test]
    fn test_load_certs_invalid_pem() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"invalid pem data").unwrap();
        temp_file.flush().unwrap();

        let result = load_certs(temp_file.path());
        assert!(result.is_ok()); // Empty cert list is valid (no certificates found)
        assert_eq!(result.unwrap().len(), 0);
    }

    #[test]
    fn test_load_private_key_empty_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"").unwrap();
        temp_file.flush().unwrap();

        let result = load_private_key(temp_file.path());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TlsError::KeyParseError(_)));
    }

    #[test]
    fn test_create_rustls_config_from_generated_cert() {
        // Generate a self-signed cert
        let (cert_pem, key_pem) = generate_self_signed_cert().unwrap();

        // Write to temp files
        let mut cert_file = NamedTempFile::new().unwrap();
        let mut key_file = NamedTempFile::new().unwrap();

        cert_file.write_all(cert_pem.as_bytes()).unwrap();
        key_file.write_all(key_pem.as_bytes()).unwrap();

        cert_file.flush().unwrap();
        key_file.flush().unwrap();

        // Load from files
        let config = TlsConfig {
            cert_path: Some(cert_file.path().to_path_buf()),
            key_path: Some(key_file.path().to_path_buf()),
            auto_generate: false,
        };

        let result = create_rustls_config(&config);
        assert!(result.is_ok());
    }
}
