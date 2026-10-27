//! S/MIME (RFC 8551) con las cajas de RustCrypto (`cms`, `x509-cert`, `der`, `rsa`, `p256`,
//! `p384`): verificar firmas CMS `SignedData` (separadas u opacas), comprobar la cadena del
//! firmante contra las raíces de confianza, firmar con un certificado `.p12`, cifrar
//! (`EnvelopedData`, RSA + AES-256-CBC) y descifrar.
//!
//! Raíces de confianza: las del sistema (archivos PEM conocidos de Linux/macOS/BSD) y, si no hay,
//! las de Mozilla de `webpki-roots`, más las que el usuario agregue.

use std::time::{Duration, SystemTime};

use cms::builder::{
    ContentEncryptionAlgorithm, EnvelopedDataBuilder, KeyEncryptionInfo, KeyTransRecipientInfoBuilder,
    SignedDataBuilder, SignerInfoBuilder,
};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::ContentInfo;
use cms::enveloped_data::{EnvelopedData, RecipientIdentifier, RecipientInfo};
use cms::signed_data::{EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo};
use const_oid::ObjectIdentifier;
use const_oid::db::{rfc5911, rfc5912};
use der::asn1::OctetString;
use der::{Any, Decode, DecodePem, Encode};
use rsa::pkcs8::DecodePrivateKey;
use rsa::{Pkcs1v15Encrypt, Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256, Sha384, Sha512};
use signature::hazmat::PrehashVerifier;
use spki::{DecodePublicKey, SubjectPublicKeyInfoOwned};
use x509_cert::Certificate;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;

use crate::error::{CryptoError, Result, guard};
use crate::report::{SignatureInfo, SignatureState};

/// Tamaño máximo de una estructura CMS que se interpreta.
pub const MAX_CMS_BYTES: usize = 64 * 1024 * 1024;
/// Profundidad máxima de una cadena de certificados.
const MAX_CHAIN: usize = 8;

const OID_EMAIL: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.1");
const OID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const OID_SAN: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.17");
const OID_RSAES_OAEP: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.7");
const OID_SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");
const OID_SHA1_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.5");

/// Algoritmo de resumen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => sha1::Sha1::digest(data).to_vec(),
            Hash::Sha256 => Sha256::digest(data).to_vec(),
            Hash::Sha384 => Sha384::digest(data).to_vec(),
            Hash::Sha512 => Sha512::digest(data).to_vec(),
        }
    }

    /// Resumen de un OID de resumen o de firma (`sha256WithRSAEncryption`, `ecdsa-with-SHA384`…).
    fn from_oid(oid: &ObjectIdentifier) -> Option<Hash> {
        let o = *oid;
        if o == rfc5912::ID_SHA_256 || o == rfc5912::SHA_256_WITH_RSA_ENCRYPTION || o == rfc5912::ECDSA_WITH_SHA_256 {
            Some(Hash::Sha256)
        } else if o == rfc5912::ID_SHA_384
            || o == rfc5912::SHA_384_WITH_RSA_ENCRYPTION
            || o == rfc5912::ECDSA_WITH_SHA_384
        {
            Some(Hash::Sha384)
        } else if o == rfc5912::ID_SHA_512
            || o == rfc5912::SHA_512_WITH_RSA_ENCRYPTION
            || o == rfc5912::ECDSA_WITH_SHA_512
        {
            Some(Hash::Sha512)
        } else if o == OID_SHA1 || o == OID_SHA1_RSA {
            Some(Hash::Sha1)
        } else {
            None
        }
    }
}

/// Verifica una firma «cruda» (`sig`) sobre `msg` con la clave pública `spki`.
fn verify_raw(spki: &SubjectPublicKeyInfoOwned, hash: Hash, msg: &[u8], sig: &[u8]) -> Result<()> {
    let h = hash.digest(msg);
    let alg = spki.algorithm.oid;
    let der = spki.to_der()?;
    if alg == rfc5912::RSA_ENCRYPTION {
        let pk = RsaPublicKey::from_public_key_der(&der).map_err(|e| CryptoError::BadKey(e.to_string()))?;
        let scheme = match hash {
            Hash::Sha1 => Pkcs1v15Sign::new::<sha1::Sha1>(),
            Hash::Sha256 => Pkcs1v15Sign::new::<Sha256>(),
            Hash::Sha384 => Pkcs1v15Sign::new::<Sha384>(),
            Hash::Sha512 => Pkcs1v15Sign::new::<Sha512>(),
        };
        return pk.verify(scheme, &h, sig).map_err(|_| CryptoError::Malformed("firma RSA no válida".into()));
    }
    if alg == rfc5912::ID_EC_PUBLIC_KEY {
        let curve = spki.algorithm.parameters.as_ref().and_then(|p| p.decode_as::<ObjectIdentifier>().ok());
        if curve == Some(rfc5912::SECP_256_R_1) {
            let vk = p256::ecdsa::VerifyingKey::from_public_key_der(&der).map_err(|e| CryptoError::BadKey(e.to_string()))?;
            let s = p256::ecdsa::Signature::from_der(sig).map_err(|e| CryptoError::Malformed(e.to_string()))?;
            return vk.verify_prehash(&h, &s).map_err(|_| CryptoError::Malformed("firma ECDSA no válida".into()));
        }
        if curve == Some(rfc5912::SECP_384_R_1) {
            let vk = p384::ecdsa::VerifyingKey::from_public_key_der(&der).map_err(|e| CryptoError::BadKey(e.to_string()))?;
            let s = p384::ecdsa::Signature::from_der(sig).map_err(|e| CryptoError::Malformed(e.to_string()))?;
            return vk.verify_prehash(&h, &s).map_err(|_| CryptoError::Malformed("firma ECDSA no válida".into()));
        }
        return Err(CryptoError::Unsupported("curva elíptica".into()));
    }
    Err(CryptoError::Unsupported(format!("clave {alg}")))
}

/// Raíz de confianza: sujeto y clave pública (DER completos).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub subject: Vec<u8>,
    pub spki: Vec<u8>,
}

/// Envuelve `contents` en una SEQUENCE DER (las raíces de `webpki-roots` vienen sin ella).
fn wrap_sequence(contents: &[u8]) -> Vec<u8> {
    let len = contents.len();
    let mut out = vec![0x30];
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = len.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend_from_slice(&bytes);
    }
    out.extend_from_slice(contents);
    out
}

/// Raíces de confianza.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    anchors: Vec<Anchor>,
}

/// Archivos PEM con las autoridades del sistema.
const SYSTEM_BUNDLES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/cert.pem",
    "/usr/local/etc/openssl/cert.pem",
];

impl TrustStore {
    /// Vacío (pruebas).
    pub fn empty() -> Self {
        TrustStore::default()
    }

    /// Raíces del sistema; si no se encuentra ninguna, las de Mozilla (`webpki-roots`).
    pub fn system() -> Self {
        let mut t = TrustStore::default();
        for path in SYSTEM_BUNDLES {
            if let Ok(text) = std::fs::read_to_string(path) {
                t.add_pem_bundle(&text);
                if !t.anchors.is_empty() {
                    break;
                }
            }
        }
        if t.anchors.is_empty() {
            for a in webpki_roots::TLS_SERVER_ROOTS {
                t.anchors.push(Anchor {
                    subject: wrap_sequence(a.subject.as_ref()),
                    spki: wrap_sequence(a.subject_public_key_info.as_ref()),
                });
            }
        }
        t
    }

    /// Agrega todas las autoridades de un archivo PEM (hasta 2000).
    pub fn add_pem_bundle(&mut self, text: &str) {
        let mut rest = text;
        let begin = "-----BEGIN CERTIFICATE-----";
        let end = "-----END CERTIFICATE-----";
        while let Some(b) = rest.find(begin) {
            let Some(e) = rest.get(b..).and_then(|r| r.find(end)) else { break };
            let stop = b.saturating_add(e).saturating_add(end.len());
            if let Some(pem) = rest.get(b..stop)
                && let Ok(c) = Certificate::from_pem(pem.as_bytes())
            {
                self.add(&c);
            }
            rest = rest.get(stop..).unwrap_or("");
            if self.anchors.len() >= 2000 {
                break;
            }
        }
    }

    /// Agrega un certificado como raíz de confianza.
    pub fn add(&mut self, c: &Certificate) {
        if let (Ok(subject), Ok(spki)) =
            (c.tbs_certificate.subject.to_der(), c.tbs_certificate.subject_public_key_info.to_der())
        {
            self.anchors.push(Anchor { subject, spki });
        }
    }

    pub fn add_der(&mut self, der: &[u8]) -> Result<()> {
        let c = Certificate::from_der(der)?;
        self.add(&c);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// Verifica la firma de `cert` con la clave de `issuer_spki`.
fn cert_signed_by(cert: &Certificate, issuer_spki: &SubjectPublicKeyInfoOwned) -> bool {
    let Some(hash) = Hash::from_oid(&cert.signature_algorithm.oid) else { return false };
    let Ok(tbs) = cert.tbs_certificate.to_der() else { return false };
    let Some(sig) = cert.signature.as_bytes() else { return false };
    verify_raw(issuer_spki, hash, &tbs, sig).is_ok()
}

/// ¿Llega `leaf` a una raíz de confianza usando `bag` como intermedios?
fn chain_trusted(leaf: &Certificate, bag: &[Certificate], trust: &TrustStore) -> bool {
    let mut cur = leaf.clone();
    for _ in 0..MAX_CHAIN {
        let Ok(issuer) = cur.tbs_certificate.issuer.to_der() else { return false };
        for a in trust.anchors.iter().filter(|a| a.subject == issuer) {
            if let Ok(spki) = SubjectPublicKeyInfoOwned::from_der(&a.spki)
                && cert_signed_by(&cur, &spki)
            {
                return true;
            }
        }
        let next = bag.iter().find(|c| {
            c.tbs_certificate.subject.to_der().ok().as_deref() == Some(issuer.as_slice())
                && c != &&cur
                && cert_signed_by(&cur, &c.tbs_certificate.subject_public_key_info)
        });
        match next {
            Some(n) => cur = n.clone(),
            None => return false,
        }
    }
    false
}

/// Direcciones de correo de un certificado (SAN rfc822Name y `emailAddress` del sujeto).
pub fn cert_emails(c: &Certificate) -> Vec<String> {
    let mut out = Vec::new();
    for ext in c.tbs_certificate.extensions.iter().flatten() {
        if ext.extn_id == OID_SAN
            && let Ok(san) = SubjectAltName::from_der(ext.extn_value.as_bytes())
        {
            for n in san.0.iter() {
                if let GeneralName::Rfc822Name(s) = n {
                    out.push(s.as_str().to_ascii_lowercase());
                }
            }
        }
    }
    for rdn in c.tbs_certificate.subject.0.iter() {
        for atv in rdn.0.iter() {
            if atv.oid == OID_EMAIL
                && let Ok(s) = atv.value.decode_as::<der::asn1::Ia5StringRef<'_>>()
            {
                out.push(s.as_str().to_ascii_lowercase());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Nombre legible del sujeto (CN o el DN entero).
pub fn subject_label(c: &Certificate) -> String {
    let cn = ObjectIdentifier::new_unwrap("2.5.4.3");
    for rdn in c.tbs_certificate.subject.0.iter() {
        for atv in rdn.0.iter() {
            if atv.oid == cn {
                if let Ok(s) = atv.value.decode_as::<der::asn1::Utf8StringRef<'_>>() {
                    return s.as_str().to_string();
                }
                if let Ok(s) = atv.value.decode_as::<der::asn1::PrintableStringRef<'_>>() {
                    return s.as_str().to_string();
                }
            }
        }
    }
    c.tbs_certificate.subject.to_string()
}

fn issuer_label(c: &Certificate) -> String {
    c.tbs_certificate.issuer.to_string()
}

/// Huella SHA-256 del certificado (hex).
pub fn cert_fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect()
}

fn time_rfc3339(t: &x509_cert::time::Time) -> String {
    let secs = t.to_unix_duration().as_secs();
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

fn valid_now(c: &Certificate, now: SystemTime) -> bool {
    let now = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let v = &c.tbs_certificate.validity;
    v.not_before.to_unix_duration() <= now && now <= v.not_after.to_unix_duration()
}

fn certs_of(sd: &SignedData) -> Vec<Certificate> {
    sd.certificates
        .iter()
        .flat_map(|set| set.0.iter())
        .filter_map(|c| match c {
            CertificateChoices::Certificate(c) => Some(c.clone()),
            _ => None,
        })
        .take(64)
        .collect()
}

fn find_signer<'a>(si: &SignerInfo, certs: &'a [Certificate]) -> Option<&'a Certificate> {
    match &si.sid {
        SignerIdentifier::IssuerAndSerialNumber(ias) => certs
            .iter()
            .find(|c| c.tbs_certificate.serial_number == ias.serial_number && c.tbs_certificate.issuer == ias.issuer),
        SignerIdentifier::SubjectKeyIdentifier(skid) => certs.iter().find(|c| {
            c.tbs_certificate.extensions.iter().flatten().any(|e| {
                e.extn_id == ObjectIdentifier::new_unwrap("2.5.29.14")
                    && OctetString::from_der(e.extn_value.as_bytes()).ok().as_ref() == Some(&skid.0)
            })
        }),
    }
}

/// Comprueba la firma de un `SignerInfo` sobre `content`.
fn verify_signer_info(si: &SignerInfo, cert: &Certificate, content: &[u8]) -> Result<()> {
    let digest_hash = Hash::from_oid(&si.digest_alg.oid)
        .ok_or_else(|| CryptoError::Unsupported(format!("resumen {}", si.digest_alg.oid)))?;
    let sig_hash = Hash::from_oid(&si.signature_algorithm.oid).unwrap_or(digest_hash);
    let spki = &cert.tbs_certificate.subject_public_key_info;
    let sig = si.signature.as_bytes();
    match &si.signed_attrs {
        Some(attrs) => {
            let md = attrs
                .iter()
                .find(|a| a.oid == OID_MESSAGE_DIGEST)
                .and_then(|a| a.values.iter().next())
                .and_then(|v| v.decode_as::<OctetString>().ok())
                .ok_or_else(|| CryptoError::Malformed("falta el resumen firmado".into()))?;
            if md.as_bytes() != digest_hash.digest(content).as_slice() {
                return Err(CryptoError::Malformed("el contenido no corresponde a la firma".into()));
            }
            let der = attrs.to_der()?;
            verify_raw(spki, sig_hash, &der, sig)
        }
        None => verify_raw(spki, sig_hash, content, sig),
    }
}

/// Resultado de verificar un mensaje S/MIME firmado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub info: SignatureInfo,
    /// Certificado del firmante (DER), para aprenderlo y poder cifrarle después.
    pub signer_cert: Option<Vec<u8>>,
    /// Contenido firmado (en las firmas opacas viene dentro del CMS).
    pub content: Vec<u8>,
}

fn parse_signed_data(cms_der: &[u8]) -> Result<SignedData> {
    if cms_der.len() > MAX_CMS_BYTES {
        return Err(CryptoError::TooLarge(cms_der.len()));
    }
    let ci = ContentInfo::from_der(cms_der)?;
    if ci.content_type != rfc5911::ID_SIGNED_DATA {
        return Err(CryptoError::Malformed("no es una firma CMS".into()));
    }
    Ok(ci.content.decode_as::<SignedData>()?)
}

/// Verifica un `SignedData`. `detached` = contenido externo (multipart/signed); si es `None`, el
/// contenido va dentro (application/pkcs7-mime; smime-type=signed-data). `from` = dirección del
/// campo «De» para comprobar que el firmante es quien dice ser.
pub fn verify(cms_der: &[u8], detached: Option<&[u8]>, from: &str, trust: &TrustStore, now: SystemTime) -> Result<Verified> {
    guard("verificar S/MIME", || {
        let sd = parse_signed_data(cms_der)?;
        let content = match detached {
            Some(c) => c.to_vec(),
            None => sd
                .encap_content_info
                .econtent
                .as_ref()
                .map(|a| a.decode_as::<OctetString>().map(|o| o.as_bytes().to_vec()).unwrap_or_else(|_| a.value().to_vec()))
                .ok_or_else(|| CryptoError::Malformed("la firma no trae contenido".into()))?,
        };
        let certs = certs_of(&sd);
        let Some(si) = sd.signer_infos.0.iter().next() else {
            return Err(CryptoError::Malformed("la firma no tiene firmantes".into()));
        };
        let Some(cert) = find_signer(si, &certs) else {
            return Ok(Verified { info: SignatureInfo::new(SignatureState::MissingKey), signer_cert: None, content });
        };
        let mut info = SignatureInfo::new(SignatureState::Verified);
        let emails = cert_emails(cert);
        info.signer = Some(subject_label(cert));
        info.signer_addr = emails.first().cloned();
        info.issuer = Some(issuer_label(cert));
        info.valid_until = Some(time_rfc3339(&cert.tbs_certificate.validity.not_after));
        let der = cert.to_der()?;
        info.fingerprint = Some(cert_fingerprint(&der));
        if let Err(e) = verify_signer_info(si, cert, &content) {
            info.state = SignatureState::Invalid(e.to_string());
            info.detail = Some(e.to_string());
            return Ok(Verified { info, signer_cert: None, content });
        }
        let from = from.trim().to_ascii_lowercase();
        if !from.is_empty() && !emails.iter().any(|e| *e == from) {
            info.state = SignatureState::Mismatch;
            info.detail = Some(format!("certificado para {}", emails.join(", ")));
        } else if !valid_now(cert, now) {
            info.state = SignatureState::NewKey;
            info.detail = Some("certificado vencido o aún no vigente / expired or not yet valid".into());
        } else if !chain_trusted(cert, &certs, trust) {
            info.state = SignatureState::NewKey;
        }
        Ok(Verified { info, signer_cert: Some(der), content })
    })
}

/// Clave privada de una identidad S/MIME.
#[derive(Clone)]
enum PrivKey {
    Rsa(Box<RsaPrivateKey>),
    P256(p256::ecdsa::SigningKey),
}

/// Identidad S/MIME propia: certificado + clave privada (de un `.p12`).
#[derive(Clone)]
pub struct Identity {
    key: PrivKey,
    pub cert: Certificate,
    pub chain: Vec<Certificate>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("subject", &subject_label(&self.cert)).finish_non_exhaustive()
    }
}

impl Identity {
    /// Lee un `.p12`/`.pfx` (contraseña incluida).
    pub fn from_p12(data: &[u8], password: &str) -> Result<Self> {
        if data.len() > 1024 * 1024 {
            return Err(CryptoError::TooLarge(data.len()));
        }
        guard("leer .p12", || {
            let ks = p12_keystore::KeyStore::from_pkcs12(data, password).map_err(|e| {
                let s = e.to_string();
                if s.to_ascii_lowercase().contains("mac") || s.to_ascii_lowercase().contains("password") {
                    CryptoError::BadPassword
                } else {
                    CryptoError::BadKey(s)
                }
            })?;
            let (_, chain) =
                ks.private_key_chain().ok_or_else(|| CryptoError::BadKey("el archivo no trae clave privada".into()))?;
            let certs: Vec<Certificate> =
                chain.chain().iter().filter_map(|c| Certificate::from_der(c.as_der()).ok()).collect();
            let cert = certs.first().cloned().ok_or_else(|| CryptoError::BadKey("el archivo no trae certificado".into()))?;
            let key = if let Ok(k) = RsaPrivateKey::from_pkcs8_der(chain.key()) {
                PrivKey::Rsa(Box::new(k))
            } else if let Ok(k) = p256::ecdsa::SigningKey::from_pkcs8_der(chain.key()) {
                PrivKey::P256(k)
            } else {
                return Err(CryptoError::Unsupported("tipo de clave privada (se admiten RSA y P-256)".into()));
            };
            Ok(Identity { key, cert, chain: certs.into_iter().skip(1).collect() })
        })
    }

    /// Identidad a partir de piezas (pruebas y certificados creados en la app).
    pub fn from_parts_rsa(key: RsaPrivateKey, cert: Certificate) -> Self {
        Identity { key: PrivKey::Rsa(Box::new(key)), cert, chain: Vec::new() }
    }

    pub fn emails(&self) -> Vec<String> {
        cert_emails(&self.cert)
    }

    pub fn cert_der(&self) -> Result<Vec<u8>> {
        Ok(self.cert.to_der()?)
    }

    fn ias(&self) -> IssuerAndSerialNumber {
        IssuerAndSerialNumber {
            issuer: self.cert.tbs_certificate.issuer.clone(),
            serial_number: self.cert.tbs_certificate.serial_number.clone(),
        }
    }

    /// Firma separada (`SignedData` DER sin contenido) de `content`.
    pub fn sign_detached(&self, content: &[u8]) -> Result<Vec<u8>> {
        guard("firmar S/MIME", || {
            let digest = Sha256::digest(content).to_vec();
            let encap = EncapsulatedContentInfo { econtent_type: rfc5911::ID_DATA, econtent: None };
            let digest_alg = spki::AlgorithmIdentifierOwned { oid: rfc5912::ID_SHA_256, parameters: None };
            let sid = SignerIdentifier::IssuerAndSerialNumber(self.ias());
            let mut b = SignedDataBuilder::new(&encap);
            b.add_digest_algorithm(digest_alg.clone()).map_err(|e| CryptoError::Internal(e.to_string()))?;
            for c in std::iter::once(&self.cert).chain(self.chain.iter()) {
                b.add_certificate(CertificateChoices::Certificate(c.clone()))
                    .map_err(|e| CryptoError::Internal(e.to_string()))?;
            }
            match &self.key {
                PrivKey::Rsa(k) => {
                    let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new((**k).clone());
                    let si = SignerInfoBuilder::new(&signer, sid, digest_alg, &encap, Some(&digest))
                        .map_err(|e| CryptoError::Internal(e.to_string()))?;
                    b.add_signer_info::<_, rsa::pkcs1v15::Signature>(si)
                        .map_err(|e| CryptoError::Internal(e.to_string()))?;
                }
                PrivKey::P256(k) => {
                    let si = SignerInfoBuilder::new(k, sid, digest_alg, &encap, Some(&digest))
                        .map_err(|e| CryptoError::Internal(e.to_string()))?;
                    b.add_signer_info::<_, p256::ecdsa::DerSignature>(si)
                        .map_err(|e| CryptoError::Internal(e.to_string()))?;
                }
            }
            let ci = b.build().map_err(|e| CryptoError::Internal(e.to_string()))?;
            Ok(ci.to_der()?)
        })
    }

    /// Descifra un `EnvelopedData` (DER de un `ContentInfo`) dirigido a esta identidad.
    pub fn decrypt(&self, cms_der: &[u8]) -> Result<Vec<u8>> {
        if cms_der.len() > MAX_CMS_BYTES {
            return Err(CryptoError::TooLarge(cms_der.len()));
        }
        guard("descifrar S/MIME", || {
            let ci = ContentInfo::from_der(cms_der)?;
            if ci.content_type != rfc5911::ID_ENVELOPED_DATA {
                return Err(CryptoError::Malformed("no es un mensaje cifrado CMS".into()));
            }
            let ed: EnvelopedData = ci.content.decode_as()?;
            let PrivKey::Rsa(key) = &self.key else {
                return Err(CryptoError::Unsupported("descifrar con claves EC".into()));
            };
            let me = self.ias();
            let mut cek = None;
            for ri in ed.recip_infos.0.iter() {
                let RecipientInfo::Ktri(k) = ri else { continue };
                let for_me = match &k.rid {
                    RecipientIdentifier::IssuerAndSerialNumber(i) => {
                        i.issuer == me.issuer && i.serial_number == me.serial_number
                    }
                    RecipientIdentifier::SubjectKeyIdentifier(_) => true,
                };
                if !for_me {
                    continue;
                }
                let enc = k.enc_key.as_bytes();
                let r = if k.key_enc_alg.oid == OID_RSAES_OAEP {
                    key.decrypt(rsa::Oaep::new::<sha1::Sha1>(), enc)
                } else {
                    key.decrypt(Pkcs1v15Encrypt, enc)
                };
                if let Ok(c) = r {
                    cek = Some(c);
                    break;
                }
            }
            let cek = cek.ok_or_else(|| CryptoError::NoOwnKey(self.emails().join(", ")))?;
            let eci = &ed.encrypted_content;
            let iv = eci
                .content_enc_alg
                .parameters
                .as_ref()
                .and_then(|p| p.decode_as::<OctetString>().ok())
                .ok_or_else(|| CryptoError::Malformed("falta el vector de inicio".into()))?;
            let data = eci
                .encrypted_content
                .as_ref()
                .ok_or_else(|| CryptoError::Malformed("no hay contenido cifrado".into()))?
                .as_bytes();
            aes_cbc_decrypt(&eci.content_enc_alg.oid, &cek, iv.as_bytes(), data)
        })
    }
}

fn aes_cbc_decrypt(oid: &ObjectIdentifier, key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    let bad = |_| CryptoError::Malformed("clave de contenido no válida".into());
    let pad = |_| CryptoError::Malformed("relleno no válido".into());
    if *oid == rfc5911::ID_AES_128_CBC {
        cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv).map_err(bad)?.decrypt_padded_vec_mut::<Pkcs7>(data).map_err(pad)
    } else if *oid == rfc5911::ID_AES_192_CBC {
        cbc::Decryptor::<aes::Aes192>::new_from_slices(key, iv).map_err(bad)?.decrypt_padded_vec_mut::<Pkcs7>(data).map_err(pad)
    } else if *oid == rfc5911::ID_AES_256_CBC {
        cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv).map_err(bad)?.decrypt_padded_vec_mut::<Pkcs7>(data).map_err(pad)
    } else {
        Err(CryptoError::Unsupported(format!("cifrado {oid} (se admite AES-CBC)")))
    }
}

/// Cifra `content` para los certificados (DER) de `recipients` (RSA). Devuelve el `ContentInfo`
/// DER (para `application/pkcs7-mime; smime-type=enveloped-data`).
pub fn encrypt(content: &[u8], recipients: &[Vec<u8>]) -> Result<Vec<u8>> {
    if content.len() > MAX_CMS_BYTES {
        return Err(CryptoError::TooLarge(content.len()));
    }
    guard("cifrar S/MIME", || {
        let mut rng = rand::thread_rng();
        let mut infos = Vec::new();
        for der in recipients {
            let c = Certificate::from_der(der)?;
            let spki_der = c.tbs_certificate.subject_public_key_info.to_der()?;
            let pk = RsaPublicKey::from_public_key_der(&spki_der)
                .map_err(|_| CryptoError::Unsupported("cifrar a certificados no RSA".into()))?;
            let rid = RecipientIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
                issuer: c.tbs_certificate.issuer.clone(),
                serial_number: c.tbs_certificate.serial_number.clone(),
            });
            infos.push((rid, pk));
        }
        let mut rngs: Vec<rand::rngs::ThreadRng> = infos.iter().map(|_| rand::thread_rng()).collect();
        let mut b = EnvelopedDataBuilder::new(None, content, ContentEncryptionAlgorithm::Aes256Cbc, None)
            .map_err(|e| CryptoError::Internal(e.to_string()))?;
        for ((rid, pk), r) in infos.into_iter().zip(rngs.iter_mut()) {
            let ri = KeyTransRecipientInfoBuilder::new(rid, KeyEncryptionInfo::Rsa(pk), r)
                .map_err(|e| CryptoError::Internal(e.to_string()))?;
            b.add_recipient_info(ri).map_err(|e| CryptoError::Internal(e.to_string()))?;
        }
        let ed = b.build_with_rng(&mut rng).map_err(|e| CryptoError::Internal(e.to_string()))?;
        let ci = ContentInfo { content_type: rfc5911::ID_ENVELOPED_DATA, content: Any::encode_from(&ed)? };
        Ok(ci.to_der()?)
    })
}

/// ¿Es un `ContentInfo` cifrado (`EnvelopedData`) o firmado (`SignedData`)?
pub fn content_type(cms_der: &[u8]) -> Option<&'static str> {
    let ci = ContentInfo::from_der(cms_der).ok()?;
    if ci.content_type == rfc5911::ID_ENVELOPED_DATA {
        Some("enveloped-data")
    } else if ci.content_type == rfc5911::ID_SIGNED_DATA {
        Some("signed-data")
    } else {
        None
    }
}
