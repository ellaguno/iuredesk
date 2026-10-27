//! Correo seguro de punta a punta sobre mensajes RFC 5322 completos:
//!
//! - [`Crypto::seal`]: al enviar, agrega `Autocrypt` y, si se pide, firma y/o cifra con PGP/MIME
//!   (RFC 3156) o S/MIME (RFC 8551) según lo que tenga la identidad.
//! - [`Crypto::open`]: al leer, aprende claves (`Autocrypt`), descifra, verifica y devuelve el
//!   mensaje «a la vista» (lo que se pinta) más un [`SecurityReport`] para la insignia.
//!
//! El mensaje que se guarda en «Enviados» es el mismo que se manda: cifrado también para el
//! remitente, que puede volver a leerlo.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use pgp::composed::SignedPublicKey;

use crate::autocrypt::{AutocryptHeader, PreferEncrypt};
use crate::error::{CryptoError, Result};
use crate::keystore::{KeySource, KeyStore, Learned, Vault};
use crate::mime::{self, ContentType, Field};
use crate::openpgp::{self, Verification};
use crate::report::{SecurityReport, SignatureInfo, SignatureState, Technology};
use crate::smime::{self, TrustStore};

/// Profundidad máxima de capas (cifrado dentro de firma dentro de cifrado…).
const MAX_DEPTH: usize = 4;

/// Qué protección pide el usuario al enviar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SealRequest {
    pub sign: bool,
    pub encrypt: bool,
    /// Preferir S/MIME cuando la identidad tiene las dos tecnologías.
    pub prefer_smime: bool,
}

/// Mensaje abierto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// Mensaje para mostrar (descifrado, sin la envoltura de firma). Igual al original si no
    /// estaba protegido.
    pub display: Vec<u8>,
    pub report: Option<SecurityReport>,
}

/// Fachada del correo seguro.
pub struct Crypto {
    pub keys: KeyStore,
    pub trust: TrustStore,
}

impl std::fmt::Debug for Crypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Crypto").field("keys", &self.keys).field("anchors", &self.trust.len()).finish()
    }
}

/// ¿Qué puede hacer la identidad hacia estos destinatarios?
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub can_sign: bool,
    /// Se puede cifrar para todos (OpenPGP o S/MIME).
    pub can_encrypt: bool,
    /// Destinatarios sin clave.
    pub missing: Vec<String>,
    /// Todos prefieren cifrado (Autocrypt `prefer-encrypt=mutual`) y el remitente también lo usa.
    pub all_mutual: bool,
}

fn from_addr(fields: &[Field]) -> String {
    mime::header(fields, "from").map(mime::addresses).and_then(|v| v.into_iter().next()).unwrap_or_default()
}

fn message_date(fields: &[Field]) -> DateTime<Utc> {
    mime::header(fields, "date")
        .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
        .map(|d| d.with_timezone(&Utc))
        .map(|d| d.min(Utc::now()))
        .unwrap_or_else(Utc::now)
}

impl Crypto {
    /// Abre el almacén en `dir` con las raíces de confianza del sistema.
    pub fn open_dir(dir: &Path, vault: Arc<dyn Vault>) -> Result<Self> {
        Ok(Crypto { keys: KeyStore::open(dir, vault)?, trust: TrustStore::system() })
    }

    /// En memoria (pruebas).
    pub fn in_memory(vault: Arc<dyn Vault>, trust: TrustStore) -> Self {
        Crypto { keys: KeyStore::in_memory(vault), trust }
    }

    /// Capacidades de `from` hacia `recipients`.
    pub fn capabilities(&self, from: &str, recipients: &[String]) -> Capabilities {
        let pgp_own = self.keys.own_pgp(from).is_some();
        let smime_own = self.keys.own_smime(from).is_some();
        let mut missing = Vec::new();
        let mut pgp_all = pgp_own;
        let mut smime_all = smime_own;
        let mut mutual = pgp_own && !recipients.is_empty();
        for r in recipients {
            let p = self.keys.public_key_for(r).is_some_and(|k| openpgp::can_encrypt(&k));
            let s = self.keys.smime_cert_for(r).is_some();
            pgp_all &= p;
            smime_all &= s;
            mutual &= self.keys.peer(r).is_some_and(|p| p.prefer_encrypt == PreferEncrypt::Mutual);
            if !p && !s {
                missing.push(r.clone());
            }
        }
        Capabilities {
            can_sign: pgp_own || smime_own,
            can_encrypt: !recipients.is_empty() && (pgp_all || smime_all),
            missing,
            all_mutual: mutual,
        }
    }

    // --- Enviar -------------------------------------------------------------------------------

    /// Protege un mensaje completo (CRLF) antes de enviarlo. `recipients` = sobre (To+Cc+Bcc).
    pub fn seal(&self, raw: &[u8], recipients: &[String], req: SealRequest) -> Result<Vec<u8>> {
        let raw = mime::to_crlf(raw);
        let split = mime::split_message(&raw);
        let from = from_addr(&split.outer);
        let mut outer = split.outer;
        let pgp_own = self.keys.own_pgp(&from);
        // Autocrypt: siempre que haya clave propia (también sin firmar ni cifrar).
        if let Some((k, _)) = &pgp_own
            && !outer.iter().any(|f| f.is("autocrypt"))
        {
            let h = AutocryptHeader {
                addr: from.clone(),
                prefer_encrypt: PreferEncrypt::Mutual,
                keydata: openpgp::binary_public(&k.to_public_key())?,
            };
            outer.push(mime::field("Autocrypt", &h.to_value()));
        }
        if !req.sign && !req.encrypt {
            return Ok(mime::join_message(&outer, &split.entity));
        }
        let smime_own = self.keys.own_smime(&from);
        let caps = self.capabilities(&from, recipients);
        let use_smime = match (pgp_own.is_some(), smime_own.is_some()) {
            (true, true) => {
                req.prefer_smime
                    || (req.encrypt && recipients.iter().all(|r| self.keys.smime_cert_for(r).is_some())
                        && !recipients.iter().all(|r| self.keys.public_key_for(r).is_some()))
            }
            (false, true) => true,
            (true, false) => false,
            (false, false) => return Err(CryptoError::NoOwnKey(from)),
        };
        if req.encrypt && !caps.can_encrypt {
            return Err(CryptoError::MissingRecipientKeys(caps.missing));
        }
        let entity = if use_smime {
            let id = smime_own.ok_or_else(|| CryptoError::NoOwnKey(from.clone()))?;
            let mut entity = split.entity;
            if req.sign {
                entity = smime_sign_entity(&id, &entity)?;
            }
            if req.encrypt {
                let mut certs = Vec::new();
                let mut missing = Vec::new();
                for r in recipients {
                    match self.keys.smime_cert_for(r) {
                        Some(c) => certs.push(c),
                        None => missing.push(r.clone()),
                    }
                }
                if !missing.is_empty() {
                    return Err(CryptoError::MissingRecipientKeys(missing));
                }
                certs.push(id.cert_der()?);
                let cms = smime::encrypt(&entity, &certs)?;
                entity = format!(
                    "Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\n{}",
                    mime::wrap76(&STANDARD.encode(cms))
                )
                .into_bytes();
            }
            entity
        } else {
            let (key, pw) = pgp_own.ok_or_else(|| CryptoError::NoOwnKey(from.clone()))?;
            if req.encrypt {
                let mut keys: Vec<SignedPublicKey> = Vec::new();
                let mut missing = Vec::new();
                for r in recipients {
                    match self.keys.public_key_for(r) {
                        Some(k) => keys.push(k),
                        None => missing.push(r.clone()),
                    }
                }
                if !missing.is_empty() {
                    return Err(CryptoError::MissingRecipientKeys(missing));
                }
                keys.push(key.to_public_key());
                let refs: Vec<&SignedPublicKey> = keys.iter().collect();
                let signer = req.sign.then_some((&key, pw.as_str()));
                let armored = openpgp::encrypt(&split.entity, &refs, signer)?;
                let b = mime::new_boundary("enc");
                format!(
                    "Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\";\r\n boundary=\"{b}\"\r\n\r\nThis is an OpenPGP/MIME encrypted message (RFC 4880 and 3156)\r\n--{b}\r\nContent-Type: application/pgp-encrypted\r\nContent-Description: PGP/MIME version identification\r\n\r\nVersion: 1\r\n\r\n--{b}\r\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\r\nContent-Description: OpenPGP encrypted message\r\nContent-Disposition: inline; filename=\"encrypted.asc\"\r\n\r\n{}\r\n--{b}--\r\n",
                    armored.replace("\r\n", "\n").replace('\n', "\r\n").trim_end()
                )
                .into_bytes()
            } else {
                let sig = openpgp::sign_detached(&key, &pw, &split.entity)?;
                let b = mime::new_boundary("sig");
                let mut out = format!(
                    "Content-Type: multipart/signed; micalg=pgp-sha256;\r\n protocol=\"application/pgp-signature\"; boundary=\"{b}\"\r\n\r\nThis is an OpenPGP/MIME signed message (RFC 4880 and 3156)\r\n--{b}\r\n"
                )
                .into_bytes();
                out.extend_from_slice(&split.entity);
                out.extend_from_slice(
                    format!(
                        "\r\n--{b}\r\nContent-Type: application/pgp-signature; name=\"signature.asc\"\r\nContent-Description: OpenPGP digital signature\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n{}\r\n--{b}--\r\n",
                        sig.replace("\r\n", "\n").replace('\n', "\r\n").trim_end()
                    )
                    .as_bytes(),
                );
                out
            }
        };
        Ok(mime::join_message(&outer, &entity))
    }

    // --- Leer ---------------------------------------------------------------------------------

    /// Abre un mensaje recibido (o enviado). Nunca falla: lo que no se puede abrir queda en el
    /// informe y se muestra el original.
    pub fn open(&self, raw: &[u8]) -> Opened {
        if raw.len() > openpgp::MAX_MESSAGE_BYTES {
            return Opened { display: raw.to_vec(), report: None };
        }
        let crlf = mime::to_crlf(raw);
        let (head, _) = mime::split_head_body(&crlf);
        let fields = mime::parse_fields(head);
        let from = from_addr(&fields);
        let date = message_date(&fields);
        // Autocrypt del remitente: se evalúa la firma con lo que se sabía ANTES de este mensaje.
        let autocrypt = mime::header(&fields, "autocrypt")
            .and_then(AutocryptHeader::parse)
            .filter(|h| !from.is_empty() && h.addr == from)
            .and_then(|h| openpgp::parse_public(&h.keydata).ok().map(|k| (k, h.prefer_encrypt)));

        let split = mime::split_message(&crlf);
        let mut report: Option<SecurityReport> = None;
        let mut entity = split.entity.clone();
        for _ in 0..MAX_DEPTH {
            match self.open_layer(&entity, &from, autocrypt.as_ref().map(|(k, _)| k), &mut report) {
                Some(inner) => entity = inner,
                None => break,
            }
        }
        // Aprender después de evaluar.
        let mine = self.keys.own_keys().iter().any(|k| k.addrs.contains(&from));
        if let Some((k, prefer)) = &autocrypt
            && !mine
        {
            let _ = self.keys.learn(&from, k, KeySource::Autocrypt, *prefer, date);
        }
        let display = if report.is_some() { mime::join_message(&split.outer, &entity) } else { raw.to_vec() };
        Opened { display, report }
    }

    /// Abre una capa. `Some(entidad interior)` si había algo que abrir.
    fn open_layer(
        &self,
        entity: &[u8],
        from: &str,
        autocrypt: Option<&SignedPublicKey>,
        report: &mut Option<SecurityReport>,
    ) -> Option<Vec<u8>> {
        let (head, body) = mime::split_head_body(entity);
        let fields = mime::parse_fields(head);
        let ct = ContentType::of(&fields);
        let protocol = ct.param("protocol").map(str::to_ascii_lowercase).unwrap_or_default();
        match ct.mime.as_str() {
            "multipart/encrypted" if protocol == "application/pgp-encrypted" => {
                let parts = mime::split_multipart(body, ct.param("boundary")?)?;
                let armored = parts.get(1).map(|p| mime::split_head_body(p).1)?;
                let r = report.get_or_insert_with(|| SecurityReport::new(Technology::OpenPgp));
                r.encrypted = true;
                let own = self.keys.all_own_pgp();
                let own_refs: Vec<_> = own.iter().map(|(k, p)| (k, p.as_str())).collect();
                let (cands, n_known) = self.candidates(from, autocrypt);
                let refs: Vec<&SignedPublicKey> = cands.iter().collect();
                match openpgp::decrypt(armored, &own_refs, &refs) {
                    Ok(d) => {
                        if let Some(v) = d.signature {
                            r.signature = Some(self.pgp_state(v, &cands, n_known, from));
                        }
                        Some(mime::to_crlf(&d.data))
                    }
                    Err(e) => {
                        let e = if own.is_empty() { CryptoError::NoOwnKey(from.to_string()) } else { e };
                        r.decrypt_error = Some(decrypt_error_text(&e));
                        None
                    }
                }
            }
            "multipart/signed" if protocol == "application/pgp-signature" => {
                let parts = mime::split_multipart(body, ct.param("boundary")?)?;
                let signed = *parts.first()?;
                let sig = parts.get(1).map(|p| mime::split_head_body(p).1)?;
                let (cands, n_known) = self.candidates(from, autocrypt);
                let refs: Vec<&SignedPublicKey> = cands.iter().collect();
                let info = match openpgp::verify_detached(sig, signed, &refs) {
                    Ok(v) => self.pgp_state(v, &cands, n_known, from),
                    Err(e) => invalid(e.to_string()),
                };
                let r = report.get_or_insert_with(|| SecurityReport::new(Technology::OpenPgp));
                if r.signature.is_none() {
                    r.signature = Some(info);
                }
                Some(signed.to_vec())
            }
            "multipart/signed" if protocol.contains("pkcs7-signature") => {
                let parts = mime::split_multipart(body, ct.param("boundary")?)?;
                let signed = *parts.first()?;
                let sig_part = parts.get(1)?;
                let cms = decode_base64_part(sig_part)?;
                let info = self.smime_verify(&cms, Some(signed), from).map(|v| v.info);
                let r = report.get_or_insert_with(|| SecurityReport::new(Technology::Smime));
                r.signature = Some(info.unwrap_or_else(|e| invalid(e.to_string())));
                Some(signed.to_vec())
            }
            "application/pkcs7-mime" | "application/x-pkcs7-mime" => {
                let cms = decode_base64_part(entity)?;
                let kind = ct
                    .param("smime-type")
                    .map(str::to_ascii_lowercase)
                    .or_else(|| smime::content_type(&cms).map(str::to_string))
                    .unwrap_or_default();
                if kind == "enveloped-data" {
                    let r = report.get_or_insert_with(|| SecurityReport::new(Technology::Smime));
                    r.encrypted = true;
                    let ids = self.keys.all_own_smime();
                    let mut last = CryptoError::NoOwnKey(from.to_string());
                    for id in &ids {
                        match id.decrypt(&cms) {
                            Ok(plain) => return Some(mime::to_crlf(&plain)),
                            Err(e) => last = e,
                        }
                    }
                    r.decrypt_error = Some(decrypt_error_text(&last));
                    None
                } else if kind == "signed-data" {
                    match self.smime_verify(&cms, None, from) {
                        Ok(v) => {
                            let r = report.get_or_insert_with(|| SecurityReport::new(Technology::Smime));
                            r.signature = Some(v.info);
                            Some(mime::to_crlf(&v.content))
                        }
                        Err(e) => {
                            let r = report.get_or_insert_with(|| SecurityReport::new(Technology::Smime));
                            r.signature = Some(invalid(e.to_string()));
                            None
                        }
                    }
                } else {
                    None
                }
            }
            "text/plain" if report.is_none() => self.open_inline(entity, from, autocrypt, report),
            _ => None,
        }
    }

    /// PGP en línea (solo lectura): `-----BEGIN PGP SIGNED MESSAGE-----` o `…PGP MESSAGE-----`.
    fn open_inline(
        &self,
        entity: &[u8],
        from: &str,
        autocrypt: Option<&SignedPublicKey>,
        report: &mut Option<SecurityReport>,
    ) -> Option<Vec<u8>> {
        // Texto decodificado (QP/base64/charset) con mail-parser.
        let mut fake = b"From: x@x\r\n".to_vec();
        fake.extend_from_slice(entity);
        let parsed = mail_parser::MessageParser::default().parse(&fake)?;
        let text = parsed.body_text(0)?.to_string();
        let signed_at = text.find("-----BEGIN PGP SIGNED MESSAGE-----");
        let enc_at = text.find("-----BEGIN PGP MESSAGE-----");
        let (cands, n_known) = self.candidates(from, autocrypt);
        let refs: Vec<&SignedPublicKey> = cands.iter().collect();
        let mut r = SecurityReport::new(Technology::OpenPgp);
        r.inline = true;
        let plain = if let Some(start) = signed_at {
            let end_marker = "-----END PGP SIGNATURE-----";
            let end = text.get(start..)?.find(end_marker)?.saturating_add(start).saturating_add(end_marker.len());
            let block = text.get(start..end)?;
            match openpgp::verify_cleartext(block, &refs) {
                Ok((plain, v)) => {
                    r.signature = Some(self.pgp_state(v, &cands, n_known, from));
                    format!("{}{plain}{}", text.get(..start).unwrap_or(""), text.get(end..).unwrap_or(""))
                }
                Err(e) => {
                    r.signature = Some(invalid(e.to_string()));
                    text.clone()
                }
            }
        } else if let Some(start) = enc_at {
            let end_marker = "-----END PGP MESSAGE-----";
            let end = text.get(start..)?.find(end_marker)?.saturating_add(start).saturating_add(end_marker.len());
            let block = text.get(start..end)?;
            r.encrypted = true;
            let own = self.keys.all_own_pgp();
            let own_refs: Vec<_> = own.iter().map(|(k, p)| (k, p.as_str())).collect();
            match openpgp::decrypt(block.as_bytes(), &own_refs, &refs) {
                Ok(d) => {
                    if let Some(v) = d.signature {
                        r.signature = Some(self.pgp_state(v, &cands, n_known, from));
                    }
                    format!(
                        "{}{}{}",
                        text.get(..start).unwrap_or(""),
                        String::from_utf8_lossy(&d.data),
                        text.get(end..).unwrap_or("")
                    )
                }
                Err(e) => {
                    r.decrypt_error = Some(decrypt_error_text(&e));
                    text.clone()
                }
            }
        } else {
            return None;
        };
        *report = Some(r);
        let mut out = b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n".to_vec();
        out.extend_from_slice(mime::to_crlf(plain.as_bytes()).as_slice());
        Some(out)
    }

    /// Claves con las que puede venir firmado un mensaje de `from`: primero las conocidas (la del
    /// contacto o la propia), luego la de su `Autocrypt` (nueva). Devuelve (claves, n conocidas).
    fn candidates(&self, from: &str, autocrypt: Option<&SignedPublicKey>) -> (Vec<SignedPublicKey>, usize) {
        let mut v = Vec::new();
        if let Some(k) = self.keys.public_key_for(from) {
            v.push(k);
        }
        // Resto de claves conocidas (para detectar «firmado por otra persona»).
        for k in self.keys.all_public() {
            if !v.iter().any(|x| openpgp::fingerprint(x) == openpgp::fingerprint(&k)) {
                v.push(k);
            }
        }
        let n_known = v.len();
        if let Some(k) = autocrypt
            && !v.iter().any(|x| openpgp::fingerprint(x) == openpgp::fingerprint(k))
        {
            v.push(k.clone());
        }
        (v, n_known)
    }

    fn pgp_state(&self, v: Verification, cands: &[SignedPublicKey], n_known: usize, from: &str) -> SignatureInfo {
        match v {
            Verification::Good(i) => {
                let Some(k) = cands.get(i) else { return SignatureInfo::new(SignatureState::MissingKey) };
                let emails = openpgp::emails(k);
                let fpr = openpgp::fingerprint(k);
                let known_for_sender = self.keys.public_key_for(from).map(|x| openpgp::fingerprint(&x));
                let state = if !from.is_empty() && !emails.iter().any(|e| e == from) {
                    SignatureState::Mismatch
                } else if i >= n_known {
                    // Firmó con la clave de su Autocrypt: ¿había otra conocida?
                    match known_for_sender {
                        Some(old) if old != fpr => SignatureState::Mismatch,
                        _ => SignatureState::NewKey,
                    }
                } else {
                    match known_for_sender {
                        Some(old) if old == fpr => SignatureState::Verified,
                        Some(_) => SignatureState::Mismatch,
                        None => SignatureState::NewKey,
                    }
                };
                let mut info = SignatureInfo::new(state);
                info.signer = openpgp::user_label(k);
                info.signer_addr = emails.first().cloned();
                info.fingerprint = Some(openpgp::pretty_fingerprint(&fpr));
                info
            }
            Verification::Bad(e) => invalid(e),
            Verification::NoKey(hint) => {
                let mut info = SignatureInfo::new(SignatureState::MissingKey);
                info.fingerprint = hint.map(|h| openpgp::pretty_fingerprint(&h));
                info
            }
        }
    }

    fn smime_verify(&self, cms: &[u8], detached: Option<&[u8]>, from: &str) -> Result<smime::Verified> {
        let v = smime::verify(cms, detached, from, &self.trust, SystemTime::now())?;
        if let (Some(der), SignatureState::Verified | SignatureState::NewKey) = (&v.signer_cert, &v.info.state) {
            let _ = self.keys.learn_smime(from, der);
        }
        Ok(v)
    }

    /// Busca por WKD la clave de `addr` y la guarda. `Ok(true)` si se encontró.
    pub async fn wkd_fetch(&self, client: &crate::wkd::WkdClient, addr: &str) -> Result<bool> {
        match client.lookup(addr).await? {
            Some(k) => {
                let out = self.keys.learn(addr, &k, KeySource::Wkd, PreferEncrypt::NoPreference, Utc::now())?;
                Ok(out != Learned::Same || self.keys.peer(addr).is_some())
            }
            None => Ok(false),
        }
    }
}

fn invalid(e: String) -> SignatureInfo {
    let mut i = SignatureInfo::new(SignatureState::Invalid(e.clone()));
    i.detail = Some(e);
    i
}

fn decrypt_error_text(e: &CryptoError) -> (String, String) {
    match e {
        CryptoError::NoOwnKey(_) => (
            "No tienes la clave con la que se cifró este mensaje. Importa tu clave privada en Ajustes › Seguridad.".into(),
            "You don't have the key this message was encrypted to. Import your private key in Settings › Security.".into(),
        ),
        e => (e.message(true), e.message(false)),
    }
}

/// Contenido de una parte en base64 (o binaria) a bytes.
fn decode_base64_part(part: &[u8]) -> Option<Vec<u8>> {
    let (head, body) = mime::split_head_body(part);
    let fields = mime::parse_fields(head);
    let cte = mime::header(&fields, "content-transfer-encoding").unwrap_or("").to_ascii_lowercase();
    if cte.contains("base64") || body.iter().all(|b| b.is_ascii()) {
        let clean: Vec<u8> = body.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
        if let Ok(d) = STANDARD.decode(&clean) {
            return Some(d);
        }
        // PEM («-----BEGIN PKCS7-----»).
        let text = String::from_utf8_lossy(body);
        let inner: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
        return STANDARD.decode(inner.trim()).ok();
    }
    Some(body.to_vec())
}

/// Firma S/MIME (multipart/signed) de una entidad.
fn smime_sign_entity(id: &smime::Identity, entity: &[u8]) -> Result<Vec<u8>> {
    let cms = id.sign_detached(entity)?;
    let b = mime::new_boundary("smime");
    let mut out = format!(
        "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; micalg=sha-256;\r\n boundary=\"{b}\"\r\n\r\nThis is an S/MIME signed message\r\n--{b}\r\n"
    )
    .into_bytes();
    out.extend_from_slice(entity);
    out.extend_from_slice(
        format!(
            "\r\n--{b}\r\nContent-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7s\"\r\n\r\n{}\r\n--{b}--\r\n",
            mime::wrap76(&STANDARD.encode(cms)).trim_end()
        )
        .as_bytes(),
    );
    Ok(out)
}
