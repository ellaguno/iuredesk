//! Una CA «interna» de pruebas (la de una empresa, no pública) y un certificado de servidor
//! firmado por ella para `ex1.iuredesk.test` y `127.0.0.1`, generados al vuelo: ninguna clave va a
//! git y nada caduca. La prueba corre la mitad «con la CA en el sistema» en un proceso hijo con
//! `SSL_CERT_FILE` (lo lee `rustls-native-certs`), porque las raíces se leen una vez por proceso.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Variable con la carpeta de los certificados, para el proceso hijo.
const DIR_VAR: &str = "IUREDESK_TEST_TLS";

/// Carpeta con `ca.pem`, `server.pem` y `server.key` (en el hijo, la del padre).
pub fn lab_dir() -> PathBuf {
    if let Some(d) = std::env::var_os(DIR_VAR) {
        return d.into();
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("iuredesk-tls-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name.push(rcgen::DnType::CommonName, "CA interna de pruebas (IureDesk)");
    ca.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let mut sp = rcgen::CertificateParams::new(vec!["ex1.iuredesk.test".to_string(), "127.0.0.1".to_string()]).unwrap();
    sp.distinguished_name.push(rcgen::DnType::CommonName, "ex1.iuredesk.test");
    let cert = sp.signed_by(&key, &issuer).unwrap();
    std::fs::write(dir.join("ca.pem"), ca_cert.pem()).unwrap();
    std::fs::write(dir.join("server.pem"), cert.pem()).unwrap();
    std::fs::write(dir.join("server.key"), key.serialize_pem()).unwrap();
    dir
}

/// Configuración del servidor TLS con el certificado firmado por la CA interna.
pub fn server_config(dir: &Path) -> Arc<rustls::ServerConfig> {
    let certs: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(dir.join("server.pem")).unwrap().map(Result::unwrap).collect();
    let key = PrivateKeyDer::from_pem_file(dir.join("server.key")).unwrap();
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    Arc::new(cfg)
}

/// ¿Es el proceso hijo (con la CA en el «sistema»)?
pub fn in_child() -> bool {
    std::env::var_os(DIR_VAR).is_some()
}

/// Corre la prueba ignorada `test` en un proceso hijo con la CA instalada «en el sistema».
pub fn run_child(test: &str, dir: &Path) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", test, "--nocapture"])
        .env("SSL_CERT_FILE", dir.join("ca.pem"))
        .env(DIR_VAR, dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(dir);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.contains("1 passed"), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
}
