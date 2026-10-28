//! S/MIME con certificados generados en la prueba (x509-cert, Rust puro): `.p12` → firmar →
//! verificar contra una CA de confianza o no; cifrar → descifrar; alteraciones detectadas.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use der::Encode;
use iuredesk_crypto::smime::{self, TrustStore};
use iuredesk_crypto::{Crypto, MemoryVault, SealRequest, SignatureState, Technology};
use rsa::RsaPrivateKey;
use rsa::pkcs8::EncodePrivateKey;
use sha2::Sha256;
use spki::EncodePublicKey;
use x509_cert::Certificate;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::der::asn1::Ia5String;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::Validity;

struct Ca {
    key: RsaPrivateKey,
    cert: Certificate,
}

fn rsa_key() -> RsaPrivateKey {
    RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap()
}

fn spki(k: &RsaPrivateKey) -> SubjectPublicKeyInfoOwned {
    let der = k.to_public_key().to_public_key_der().unwrap();
    SubjectPublicKeyInfoOwned::try_from(der.as_bytes()).unwrap()
}

fn ca(name: &str) -> Ca {
    let key = rsa_key();
    let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new(key.clone());
    let subject = Name::from_str(&format!("CN={name},O=Pruebas")).unwrap();
    let b = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(1u32),
        Validity::from_now(Duration::from_secs(3600 * 24)).unwrap(),
        subject,
        spki(&key),
        &signer,
    )
    .unwrap();
    let cert = b.build::<rsa::pkcs1v15::Signature>().unwrap();
    Ca { key, cert }
}

fn leaf(ca: &Ca, email: &str, serial: u32) -> (RsaPrivateKey, Certificate) {
    let key = rsa_key();
    let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new(ca.key.clone());
    let mut b = CertificateBuilder::new(
        Profile::Leaf {
            issuer: ca.cert.tbs_certificate.subject.clone(),
            enable_key_agreement: false,
            enable_key_encipherment: true,
        },
        SerialNumber::from(serial),
        Validity::from_now(Duration::from_secs(3600 * 24)).unwrap(),
        Name::from_str(&format!("CN=Persona {serial}")).unwrap(),
        spki(&key),
        &signer,
    )
    .unwrap();
    b.add_extension(&SubjectAltName(vec![GeneralName::Rfc822Name(Ia5String::new(email).unwrap())])).unwrap();
    (key, b.build::<rsa::pkcs1v15::Signature>().unwrap())
}

fn p12(key: &RsaPrivateKey, cert: &Certificate, ca: &Ca, password: &str) -> Vec<u8> {
    let pkcs8 = key.to_pkcs8_der().unwrap();
    let chain = vec![
        p12_keystore::Certificate::from_der(&cert.to_der().unwrap()).unwrap(),
        p12_keystore::Certificate::from_der(&ca.cert.to_der().unwrap()).unwrap(),
    ];
    let mut ks = p12_keystore::KeyStore::new();
    ks.add_entry(
        "yo",
        p12_keystore::KeyStoreEntry::PrivateKeyChain(p12_keystore::PrivateKeyChain::new(
            pkcs8.as_bytes(),
            [1u8; 20],
            chain,
        )),
    );
    ks.writer(password).write().unwrap()
}

const MSG: &[u8] = "From: Ana <ana@despacho.mx>\r\nTo: Beto <beto@cliente.mx>\r\nSubject: Contrato\r\nDate: Tue, 27 Oct 2026 10:00:00 -0600\r\nMessage-ID: <1@despacho.mx>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\nAdjunto la versión final.\r\n".as_bytes();

#[test]
fn p12_sign_verify_trusted_and_untrusted() {
    let ca = ca("CA Pruebas");
    let (key, cert) = leaf(&ca, "ana@despacho.mx", 10);
    let file = p12(&key, &cert, &ca, "secreto");

    // Contraseña incorrecta → error humano.
    assert!(smime::Identity::from_p12(&file, "mala").is_err());

    let mut trust = TrustStore::empty();
    trust.add(&ca.cert);
    let sender = Crypto::in_memory(Arc::new(MemoryVault::default()), TrustStore::empty());
    let info = sender.keys.import_p12(&file, "secreto").unwrap();
    assert_eq!(info.addrs, vec!["ana@despacho.mx"]);
    let sealed = sender
        .seal(MSG, &["beto@cliente.mx".into()], SealRequest { sign: true, encrypt: false, prefer_smime: false })
        .unwrap();
    let text = String::from_utf8_lossy(&sealed);
    assert!(text.contains("application/pkcs7-signature") && text.contains("micalg=sha-256"));

    // Receptor que confía en la CA: Verificado.
    let reader = Crypto::in_memory(Arc::new(MemoryVault::default()), trust);
    let opened = reader.open(&sealed);
    let r = opened.report.unwrap();
    assert_eq!(r.technology, Technology::Smime);
    let sig = r.signature.unwrap();
    assert_eq!(sig.state, SignatureState::Verified, "{sig:?}");
    assert_eq!(sig.signer.as_deref(), Some("Persona 10"));
    assert!(String::from_utf8_lossy(&opened.display).contains("versión final"));
    // Y aprende el certificado para poder cifrarle después.
    assert!(reader.keys.smime_cert_for("ana@despacho.mx").is_some());

    // Receptor sin la CA: firma válida, emisor no reconocido.
    let other = Crypto::in_memory(Arc::new(MemoryVault::default()), TrustStore::empty());
    assert_eq!(other.open(&sealed).report.unwrap().signature.unwrap().state, SignatureState::NewKey);

    // Contenido alterado: verificación fallida.
    let tampered = String::from_utf8_lossy(&sealed).replace("versión final", "versión falsa");
    let st = reader.open(tampered.as_bytes()).report.unwrap().signature.unwrap().state;
    assert!(matches!(st, SignatureState::Invalid(_)), "{st:?}");

    // Remitente suplantado: el certificado es de ana@, el «De» dice otra cosa.
    let spoof = String::from_utf8_lossy(&sealed).replace("From: Ana <ana@despacho.mx>", "From: Ana <ana@otro.mx>");
    assert_eq!(reader.open(spoof.as_bytes()).report.unwrap().signature.unwrap().state, SignatureState::Mismatch);
}

#[test]
fn encrypt_and_decrypt() {
    let ca = ca("CA Cifrado");
    let (ka, ca_cert) = leaf(&ca, "ana@despacho.mx", 20);
    let (kb, cb_cert) = leaf(&ca, "beto@cliente.mx", 21);
    let ana = Crypto::in_memory(Arc::new(MemoryVault::default()), TrustStore::empty());
    ana.keys.import_p12(&p12(&ka, &ca_cert, &ca, "a"), "a").unwrap();
    let beto = Crypto::in_memory(Arc::new(MemoryVault::default()), TrustStore::empty());
    beto.keys.import_p12(&p12(&kb, &cb_cert, &ca, "b"), "b").unwrap();

    // Sin el certificado de Beto no se puede cifrar: error claro.
    let req = SealRequest { sign: true, encrypt: true, prefer_smime: false };
    let err = ana.seal(MSG, &["beto@cliente.mx".into()], req).unwrap_err();
    assert!(err.message(true).contains("beto@cliente.mx"), "{}", err.message(true));

    ana.keys.learn_smime("beto@cliente.mx", &cb_cert.to_der().unwrap()).unwrap();
    assert!(ana.capabilities("ana@despacho.mx", &["beto@cliente.mx".into()]).can_encrypt);
    let sealed = ana.seal(MSG, &["beto@cliente.mx".into()], req).unwrap();
    assert!(String::from_utf8_lossy(&sealed).contains("smime-type=enveloped-data"));
    assert!(!String::from_utf8_lossy(&sealed).contains("versión final"));

    let opened = beto.open(&sealed);
    let r = opened.report.unwrap();
    assert!(r.encrypted && r.decrypt_error.is_none(), "{r:?}");
    assert_eq!(r.signature.unwrap().state, SignatureState::NewKey); // CA no está entre las raíces
    assert!(String::from_utf8_lossy(&opened.display).contains("versión final"));
    // El remitente también puede leer su copia de «Enviados».
    assert!(ana.open(&sealed).report.unwrap().decrypt_error.is_none());
    // Un tercero no: error en lenguaje humano.
    let eve = Crypto::in_memory(Arc::new(MemoryVault::default()), TrustStore::empty());
    let r = eve.open(&sealed).report.unwrap();
    assert!(r.decrypt_error.is_some());
    assert_eq!(r.title(true), "No se pudo descifrar");
}

#[test]
fn garbage_cms_never_panics() {
    let t = TrustStore::empty();
    for data in [&b""[..], b"\x30\x80", b"\x30\x03\x02\x01\x01", &[0xffu8; 64]] {
        assert!(smime::verify(data, Some(b"x"), "a@b.c", &t, std::time::SystemTime::now()).is_err());
        assert!(smime::content_type(data).is_none());
    }
    let c = Crypto::in_memory(Arc::new(MemoryVault::default()), t);
    let raw = b"From: a@b.c\r\nContent-Type: application/pkcs7-mime; smime-type=signed-data\r\nContent-Transfer-Encoding: base64\r\n\r\nMIIBADAN\r\n";
    let r = c.open(raw).report.unwrap();
    assert!(matches!(r.signature.unwrap().state, SignatureState::Invalid(_)));
}
