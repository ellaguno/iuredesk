//! Kerberos propio (RFC 4120) para Exchange local cuando NTLM no basta (F8.2): dominios que
//! restringen NTLM o servidores que solo ofrecen `Negotiate`.
//!
//! Con usuario y contraseña, sin depender del sistema:
//! 1. **AS-REQ** al KDC del dominio (TCP 88): la primera sin preautenticación para que el KDC diga
//!    el tipo de cifrado y la sal (`ETYPE-INFO2`); la segunda con `PA-ENC-TIMESTAMP`. El AS-REP
//!    trae el TGT y su clave de sesión. Usuario `ana@contoso.com` va como *enterprise principal*
//!    (el KDC lo traduce a su cuenta y dice el reino verdadero); `DOMINIO\ana` como principal
//!    normal.
//! 2. **TGS-REQ** por `HTTP/<servidor>`: el ticket del servicio y su clave.
//! 3. Cada petición HTTP lleva un **AP-REQ** nuevo dentro de SPNEGO (`Negotiate <base64>`), con el
//!    checksum GSS (0x8003) que incluye el *channel binding* `tls-server-end-point` para la
//!    *Extended Protection* de Exchange.
//!
//! El KDC se busca en el nombre DNS del reino (en Active Directory resuelve a los controladores
//! de dominio): sin red hacia el dominio (fuera de la oficina y sin VPN) no hay KDC y se sigue con
//! NTLM. Solo AES (17 y 18); RC4 no. No se verifica el AP-REP (autenticación mutua): el canal ya
//! es TLS con certificado verificado.
//!
//! Todo lo que llega del KDC es hostil: DER acotado, respuestas ≤ 1 MiB, saltos de reino acotados.

pub mod crypto;
pub mod der;

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::BoxFuture;
use crypto::Key;
use der::{app, bits32, boolean, ctx, gstr, gtime, int, octets, seq};

/// Tamaño máximo de una respuesta del KDC.
pub const MAX_REPLY: usize = 1024 * 1024;
/// Reinos que se prueban como mucho (candidatos y referencias).
const MAX_REALMS: usize = 4;
/// Margen antes del vencimiento del ticket para pedir otro.
const RENEW_MARGIN: Duration = Duration::from_secs(5 * 60);

/// `NT-PRINCIPAL`.
const NT_PRINCIPAL: i64 = 1;
/// `NT-SRV-INST`.
const NT_SRV_INST: i64 = 2;
/// `NT-ENTERPRISE` (RFC 6806).
const NT_ENTERPRISE: i64 = 10;
/// forwardable, renewable, canonicalize.
const KDC_OPTIONS: u32 = 0x4081_0000;
/// Tipos de cifrado que se piden (el KDC elige el primero que tenga).
const ETYPES: [i32; 2] = [crypto::AES256, crypto::AES128];

/// OID de Kerberos 5 (1.2.840.113554.1.2.2).
const OID_KRB5: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02];
/// OID de Kerberos 5 de Microsoft (1.2.840.48018.1.2.2).
const OID_MS_KRB5: [u8; 9] = [0x2a, 0x86, 0x48, 0x82, 0xf7, 0x12, 0x01, 0x02, 0x02];
/// OID de SPNEGO (1.3.6.1.5.5.2).
const OID_SPNEGO: [u8; 6] = [0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];

/// Fallo de Kerberos.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KerberosError {
    /// No hay KDC alcanzable (fuera de la red del dominio).
    #[error("no se alcanza el controlador de dominio: {0}")]
    Unreachable(String),
    /// Contraseña equivocada (`KDC_ERR_PREAUTH_FAILED`).
    #[error("contraseña rechazada")]
    BadPassword,
    /// El dominio no conoce al usuario.
    #[error("usuario desconocido para el dominio")]
    UnknownUser,
    /// La contraseña caducó y hay que cambiarla.
    #[error("la contraseña caducó")]
    PasswordExpired,
    /// Cuenta bloqueada o desactivada.
    #[error("cuenta bloqueada o desactivada")]
    Locked,
    /// La hora del equipo y la del dominio difieren más de lo permitido (5 min).
    #[error("la hora del equipo no coincide con la del dominio")]
    ClockSkew,
    /// El dominio no tiene el servicio (`HTTP/servidor` sin registrar).
    #[error("el dominio no conoce el servicio {0}")]
    NoService(String),
    /// El usuario está en otro reino.
    #[error("el usuario está en el reino {0}")]
    WrongRealm(String),
    /// HMAC que no cuadra (clave equivocada o mensaje alterado).
    #[error("integridad")]
    Integrity,
    /// Otro error del KDC.
    #[error("KDC {code}: {text}")]
    Kdc { code: i64, text: String },
    /// Respuesta mal formada o inesperada.
    #[error("kerberos: {0}")]
    Protocol(String),
}

/// Un nombre de principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub kind: i64,
    pub names: Vec<String>,
}

impl Principal {
    fn encode(&self) -> Vec<u8> {
        seq(&[ctx(0, int(self.kind)), ctx(1, seq(&self.names.iter().map(|n| gstr(n)).collect::<Vec<_>>()))])
    }

    fn decode(t: der::Tlv<'_>) -> Result<Self, KerberosError> {
        let f = der::fields(t)?;
        let kind = der::need(&f, 0, "name-type")?.int()?;
        let names = der::children(der::need(&f, 1, "name-string")?.body)?
            .iter()
            .take(8)
            .map(der::Tlv::string)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { kind, names })
    }

    /// Principal del usuario: `ana@contoso.com` como *enterprise*, `ana` como normal.
    pub fn user(name: &str) -> Self {
        if name.contains('@') {
            Self { kind: NT_ENTERPRISE, names: vec![name.to_string()] }
        } else {
            Self { kind: NT_PRINCIPAL, names: vec![name.to_string()] }
        }
    }
}

/// Un ticket con su clave de sesión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// Reino y nombre del cliente según el KDC (canónicos).
    pub crealm: String,
    pub cname: Principal,
    /// El `Ticket` tal cual (DER), para el AP-REQ.
    pub ticket: Vec<u8>,
    pub key: Key,
    pub end: DateTime<Utc>,
}

impl Ticket {
    /// ¿Sigue vigente con margen?
    pub fn fresh(&self, now: DateTime<Utc>) -> bool {
        chrono::Duration::from_std(RENEW_MARGIN).is_ok_and(|m| now + m < self.end)
    }
}

/// `AAAAMMDDHHMMSSZ`.
fn ktime(t: DateTime<Utc>) -> String {
    t.format("%Y%m%d%H%M%SZ").to_string()
}

fn parse_ktime(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%SZ").ok().map(|n| n.and_utc())
}

fn pa_data(kind: i64, value: Vec<u8>) -> Vec<u8> {
    seq(&[ctx(1, int(kind)), ctx(2, octets(&value))])
}

fn encrypted_data(etype: i32, cipher: Vec<u8>) -> Vec<u8> {
    seq(&[ctx(0, int(i64::from(etype))), ctx(2, octets(&cipher))])
}

/// `KDC-REQ-BODY`.
fn req_body(cname: Option<&Principal>, realm: &str, sname: &Principal, till: DateTime<Utc>, nonce: u32) -> Vec<u8> {
    let mut f = vec![ctx(0, bits32(KDC_OPTIONS))];
    if let Some(c) = cname {
        f.push(ctx(1, c.encode()));
    }
    f.push(ctx(2, gstr(realm)));
    f.push(ctx(3, sname.encode()));
    f.push(ctx(5, gtime(&ktime(till))));
    f.push(ctx(7, int(i64::from(nonce))));
    f.push(ctx(8, seq(&ETYPES.iter().map(|e| int(i64::from(*e))).collect::<Vec<_>>())));
    seq(&f)
}

/// AS-REQ. `padata` va tal cual (lista de `PA-DATA`).
pub fn as_req(user: &Principal, realm: &str, padata: Vec<Vec<u8>>, now: DateTime<Utc>, nonce: u32) -> Vec<u8> {
    let krbtgt = Principal { kind: NT_SRV_INST, names: vec!["krbtgt".into(), realm.to_string()] };
    let body = req_body(Some(user), realm, &krbtgt, now + chrono::Duration::hours(10), nonce);
    let mut f = vec![ctx(1, int(5)), ctx(2, int(10))];
    if !padata.is_empty() {
        f.push(ctx(3, seq(&padata)));
    }
    f.push(ctx(4, body));
    app(10, seq(&f))
}

/// `PA-ENC-TIMESTAMP` (2) con la clave del usuario (uso 1).
pub fn pa_enc_timestamp(key: &Key, now: DateTime<Utc>) -> Result<Vec<u8>, KerberosError> {
    let ts = seq(&[ctx(0, gtime(&ktime(now))), ctx(1, int(i64::from(now.timestamp_subsec_micros())))]);
    let cipher = crypto::encrypt(key, 1, &ts)?;
    Ok(pa_data(2, encrypted_data(key.etype, cipher)))
}

/// `PA-PAC-REQUEST` (128): que el ticket lleve el PAC (lo exige Exchange para los grupos).
fn pa_pac_request() -> Vec<u8> {
    pa_data(128, seq(&[ctx(0, boolean(true))]))
}

/// Un `AP-REQ` con el ticket dado y un autenticador nuevo cifrado con `usage`.
pub fn ap_req(
    t: &Ticket,
    cksum: Option<(i64, Vec<u8>)>,
    usage: u32,
    now: DateTime<Utc>,
) -> Result<Vec<u8>, KerberosError> {
    let mut f = vec![ctx(0, int(5)), ctx(1, gstr(&t.crealm)), ctx(2, t.cname.encode())];
    if let Some((kind, value)) = cksum {
        f.push(ctx(3, seq(&[ctx(0, int(kind)), ctx(1, octets(&value))])));
    }
    f.push(ctx(4, int(i64::from(now.timestamp_subsec_micros()))));
    f.push(ctx(5, gtime(&ktime(now))));
    let authenticator = app(2, seq(&f));
    let cipher = crypto::encrypt(&t.key, usage, &authenticator)?;
    Ok(app(
        14,
        seq(&[
            ctx(0, int(5)),
            ctx(1, int(14)),
            ctx(2, bits32(0)),
            ctx(3, t.ticket.clone()),
            ctx(4, encrypted_data(t.key.etype, cipher)),
        ]),
    ))
}

/// TGS-REQ por `service` con el TGT.
pub fn tgs_req(tgt: &Ticket, service: &Principal, now: DateTime<Utc>, nonce: u32) -> Result<Vec<u8>, KerberosError> {
    let body = req_body(None, &tgt.crealm, service, now + chrono::Duration::hours(10), nonce);
    // Checksum del cuerpo (uso 6) en el autenticador (uso 7).
    let sum = crypto::checksum(&tgt.key, 6, &body)?;
    let ap = ap_req(tgt, Some((i64::from(crypto::checksum_type(tgt.key.etype)), sum)), 7, now)?;
    Ok(app(12, seq(&[ctx(1, int(5)), ctx(2, int(12)), ctx(3, seq(&[pa_data(1, ap)])), ctx(4, body)])))
}

/// Checksum GSS (RFC 4121 §4.1.1): longitud 16, hash del *channel binding* (o ceros) y banderas.
pub fn gss_checksum(channel_binding: Option<[u8; 16]>) -> Vec<u8> {
    let mut v = 16u32.to_le_bytes().to_vec();
    v.extend_from_slice(&channel_binding.unwrap_or([0; 16]));
    // GSS_C_MUTUAL_FLAG no: no se espera AP-REP. GSS_C_INTEG_FLAG | GSS_C_SEQUENCE_FLAG, como los
    // clientes de Windows por HTTP.
    v.extend_from_slice(&0x0000_0028u32.to_le_bytes());
    v
}

/// Token SPNEGO `NegTokenInit` con el AP-REQ dentro del token GSS de Kerberos.
pub fn spnego(ap_req: &[u8]) -> Vec<u8> {
    let mut krb = der::tlv(0x06, &OID_KRB5);
    krb.extend_from_slice(&[0x01, 0x00]); // TOK_ID de AP-REQ
    krb.extend_from_slice(ap_req);
    let gss = der::tlv(0x60, &krb);
    let mechs = seq(&[der::tlv(0x06, &OID_MS_KRB5), der::tlv(0x06, &OID_KRB5)]);
    let init = seq(&[ctx(0, mechs), ctx(2, octets(&gss))]);
    let mut inner = der::tlv(0x06, &OID_SPNEGO);
    inner.extend_from_slice(&ctx(0, init));
    der::tlv(0x60, &inner)
}

/// Lo que respondió el KDC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// AS-REP o TGS-REP.
    Rep { crealm: String, cname: Principal, ticket: Vec<u8>, etype: i32, cipher: Vec<u8> },
    /// KRB-ERROR.
    Error { code: i64, text: String, crealm: Option<String>, e_data: Vec<u8> },
}

/// Lee un AS-REP (11), TGS-REP (13) o KRB-ERROR (30).
pub fn parse_reply(b: &[u8]) -> Result<Reply, KerberosError> {
    if b.len() > MAX_REPLY {
        return Err(KerberosError::Protocol("respuesta demasiado grande".into()));
    }
    let (t, _) = der::read(b)?;
    let f = der::fields(t)?;
    match t.tag {
        0x6B | 0x6D => {
            let enc = der::fields(der::need(&f, 6, "enc-part")?)?;
            Ok(Reply::Rep {
                crealm: der::need(&f, 3, "crealm")?.string()?,
                cname: Principal::decode(der::need(&f, 4, "cname")?)?,
                ticket: der::need(&f, 5, "ticket")?.raw.to_vec(),
                etype: i32::try_from(der::need(&enc, 0, "etype")?.int()?)
                    .map_err(|_| KerberosError::Protocol("etype".into()))?,
                cipher: der::need(&enc, 2, "cipher")?.octets()?.to_vec(),
            })
        }
        0x7E => Ok(Reply::Error {
            code: der::need(&f, 6, "error-code")?.int()?,
            text: der::field(&f, 11).and_then(|t| t.string().ok()).unwrap_or_default().chars().take(200).collect(),
            crealm: der::field(&f, 7).and_then(|t| t.string().ok()),
            e_data: der::field(&f, 12).and_then(|t| t.octets().ok().map(<[u8]>::to_vec)).unwrap_or_default(),
        }),
        other => Err(KerberosError::Protocol(format!("respuesta inesperada del KDC ({other:#x})"))),
    }
}

/// Parte cifrada de un AS-REP/TGS-REP: clave de sesión, nonce y vencimiento.
pub fn parse_enc_rep(plain: &[u8]) -> Result<(Key, u32, DateTime<Utc>), KerberosError> {
    let (t, _) = der::read(plain)?;
    // EncASRepPart [25] o EncTGSRepPart [26] (AD a veces manda 25 en ambos).
    if !matches!(t.tag, 0x79 | 0x7A) {
        return Err(KerberosError::Protocol(format!("parte cifrada inesperada ({:#x})", t.tag)));
    }
    let f = der::fields(t)?;
    let key = der::fields(der::need(&f, 0, "key")?)?;
    let etype =
        i32::try_from(der::need(&key, 0, "keytype")?.int()?).map_err(|_| KerberosError::Protocol("keytype".into()))?;
    let bytes = der::need(&key, 1, "keyvalue")?.octets()?.to_vec();
    if crypto::key_len(etype) != Some(bytes.len()) {
        return Err(KerberosError::Protocol(format!("clave de sesión no válida ({etype})")));
    }
    let nonce =
        u32::try_from(der::need(&f, 2, "nonce")?.int()?).map_err(|_| KerberosError::Protocol("nonce".into()))?;
    let end = parse_ktime(&der::need(&f, 7, "endtime")?.string()?)
        .ok_or_else(|| KerberosError::Protocol("endtime".into()))?;
    Ok((Key { etype, bytes }, nonce, end))
}

/// `ETYPE-INFO2` del `e-data` de `KDC_ERR_PREAUTH_REQUIRED`: (tipo, sal, parámetros).
pub fn etype_info2(e_data: &[u8]) -> Vec<(i32, Option<Vec<u8>>, Option<Vec<u8>>)> {
    let Ok((t, _)) = der::read(e_data) else { return Vec::new() };
    let Ok(pas) = der::children(t.body) else { return Vec::new() };
    let mut out = Vec::new();
    for pa in pas {
        let Ok(f) = der::fields(pa) else { continue };
        if der::field(&f, 1).and_then(|t| t.int().ok()) != Some(19) {
            continue;
        }
        let Some(value) = der::field(&f, 2).and_then(|t| t.octets().ok()) else { continue };
        let Ok((list, _)) = der::read(value) else { continue };
        for entry in der::children(list.body).unwrap_or_default().into_iter().take(8) {
            let Ok(e) = der::fields(entry) else { continue };
            let Some(etype) = der::field(&e, 0).and_then(|t| t.int().ok()).and_then(|v| i32::try_from(v).ok()) else {
                continue;
            };
            let salt = der::field(&e, 1).map(|t| t.body.to_vec());
            let params = der::field(&e, 2).and_then(|t| t.octets().ok().map(<[u8]>::to_vec));
            out.push((etype, salt, params));
        }
    }
    out
}

/// Traduce un KRB-ERROR.
fn kdc_error(code: i64, text: String, crealm: Option<String>, service: &str) -> KerberosError {
    match code {
        6 => KerberosError::UnknownUser,
        7 => KerberosError::NoService(service.to_string()),
        18 => KerberosError::Locked,
        23 => KerberosError::PasswordExpired,
        24 => KerberosError::BadPassword,
        37 => KerberosError::ClockSkew,
        68 => match crealm {
            Some(r) => KerberosError::WrongRealm(r),
            None => KerberosError::Kdc { code, text },
        },
        _ => KerberosError::Kdc { code, text },
    }
}

/// El transporte hacia el KDC de un reino.
pub trait Kdc: Send + Sync {
    /// Manda `req` al KDC de `realm` y devuelve la respuesta.
    fn exchange<'a>(&'a self, realm: &'a str, req: Vec<u8>) -> BoxFuture<'a, Result<Vec<u8>, KerberosError>>;
}

/// KDC por TCP en el puerto 88 del nombre DNS del reino (en Active Directory, los controladores
/// de dominio). Mensajes con longitud de 4 bytes delante (RFC 4120 §7.2.2).
pub struct TcpKdc {
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
}

impl Default for TcpKdc {
    fn default() -> Self {
        Self { connect_timeout: Duration::from_secs(4), io_timeout: Duration::from_secs(10) }
    }
}

impl Kdc for TcpKdc {
    fn exchange<'a>(&'a self, realm: &'a str, req: Vec<u8>) -> BoxFuture<'a, Result<Vec<u8>, KerberosError>> {
        Box::pin(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let host = format!("{}:88", realm.to_ascii_lowercase());
            let unreachable = |e: String| KerberosError::Unreachable(format!("{host}: {e}"));
            let mut s = tokio::time::timeout(self.connect_timeout, tokio::net::TcpStream::connect(&host))
                .await
                .map_err(|_| unreachable("tiempo agotado".into()))?
                .map_err(|e| unreachable(e.to_string()))?;
            let io = async {
                let len = u32::try_from(req.len()).map_err(|_| KerberosError::Protocol("petición enorme".into()))?;
                let mut msg = len.to_be_bytes().to_vec();
                msg.extend_from_slice(&req);
                s.write_all(&msg).await.map_err(|e| unreachable(e.to_string()))?;
                let mut head = [0u8; 4];
                s.read_exact(&mut head).await.map_err(|e| unreachable(e.to_string()))?;
                let n = usize::try_from(u32::from_be_bytes(head)).unwrap_or(usize::MAX);
                if n > MAX_REPLY {
                    return Err(KerberosError::Protocol("respuesta del KDC demasiado grande".into()));
                }
                let mut body = vec![0u8; n];
                s.read_exact(&mut body).await.map_err(|e| unreachable(e.to_string()))?;
                Ok(body)
            };
            tokio::time::timeout(self.io_timeout, io).await.map_err(|_| unreachable("tiempo agotado".into()))?
        })
    }
}

fn nonce() -> Result<u32, KerberosError> {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).map_err(|e| KerberosError::Protocol(e.to_string()))?;
    Ok(u32::from_be_bytes(b) & 0x7FFF_FFFF)
}

/// Inicio de sesión en un reino: el TGT.
async fn login(kdc: &dyn Kdc, realm: &str, user: &Principal, password: &str) -> Result<Ticket, KerberosError> {
    let now = Utc::now();
    // 1. Sin preautenticación: el KDC dice tipo de cifrado y sal.
    let first = kdc.exchange(realm, as_req(user, realm, vec![pa_pac_request()], now, nonce()?)).await?;
    let (etype, salt, params) = match parse_reply(&first)? {
        Reply::Error { code: 25, e_data, .. } => etype_info2(&e_data)
            .into_iter()
            .find(|(e, _, _)| crypto::key_len(*e).is_some())
            .ok_or_else(|| KerberosError::Protocol("el dominio no ofrece AES".into()))?,
        Reply::Error { code, text, crealm, .. } => return Err(kdc_error(code, text, crealm, "")),
        // Cuenta sin preautenticación (raro): la respuesta ya está cifrada con la clave del
        // usuario; se repite con la preautenticación para tener la sal correcta.
        Reply::Rep { .. } => (crypto::AES256, None, None),
    };
    let salt = salt.unwrap_or_else(|| format!("{realm}{}", user.names.concat()).into_bytes());
    let key = crypto::string_to_key(etype, password, &salt, params.as_deref())?;
    // 2. Con la marca de tiempo cifrada.
    let n = nonce()?;
    let req = as_req(user, realm, vec![pa_enc_timestamp(&key, now)?, pa_pac_request()], now, n);
    match parse_reply(&kdc.exchange(realm, req).await?)? {
        Reply::Rep { crealm, cname, ticket, etype, cipher } => {
            if etype != key.etype {
                return Err(KerberosError::Protocol("el KDC cambió el cifrado".into()));
            }
            let plain = crypto::decrypt(&key, 3, &cipher).map_err(|e| match e {
                KerberosError::Integrity => KerberosError::BadPassword,
                other => other,
            })?;
            let (session, got, end) = parse_enc_rep(&plain)?;
            if got != n {
                return Err(KerberosError::Protocol("nonce distinto en el AS-REP".into()));
            }
            Ok(Ticket { crealm, cname, ticket, key: session, end })
        }
        Reply::Error { code, text, crealm, .. } => Err(kdc_error(code, text, crealm, "")),
    }
}

/// Ticket de servicio por `service` (p. ej. `HTTP/mail.contoso.com`).
async fn service(kdc: &dyn Kdc, tgt: &Ticket, service: &str) -> Result<Ticket, KerberosError> {
    let sname = Principal { kind: NT_SRV_INST, names: service.split('/').map(str::to_string).collect() };
    let n = nonce()?;
    let req = tgs_req(tgt, &sname, Utc::now(), n)?;
    match parse_reply(&kdc.exchange(&tgt.crealm, req).await?)? {
        Reply::Rep { crealm, cname, ticket, cipher, .. } => {
            let plain = crypto::decrypt(&tgt.key, 8, &cipher)?;
            let (key, got, end) = parse_enc_rep(&plain)?;
            if got != n {
                return Err(KerberosError::Protocol("nonce distinto en el TGS-REP".into()));
            }
            Ok(Ticket { crealm, cname, ticket, key, end })
        }
        Reply::Error { code, text, crealm, .. } => Err(kdc_error(code, text, crealm, service)),
    }
}

/// Reinos posibles: el sufijo del UPN o el dominio de `DOMINIO\usuario` si tiene puntos, y los
/// dominios padre del servidor (`mail.corp.contoso.com` → `CORP.CONTOSO.COM`, `CONTOSO.COM`).
pub fn realm_candidates(login: &str, server_host: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |r: &str| {
        let r = r.trim().trim_end_matches('.').to_ascii_uppercase();
        if r.contains('.') && !r.starts_with('.') && r.len() <= 253 && !out.contains(&r) && out.len() < MAX_REALMS {
            out.push(r);
        }
    };
    if let Some((_, d)) = login.rsplit_once('@') {
        add(d);
    }
    if let Some((d, _)) = login.split_once('\\') {
        add(d);
    }
    let labels: Vec<&str> = server_host.split('.').filter(|l| !l.is_empty()).collect();
    if server_host.parse::<std::net::IpAddr>().is_err() {
        for i in 1..labels.len().saturating_sub(1) {
            add(&labels.get(i..).unwrap_or_default().join("."));
        }
    }
    out
}

/// Ticket de servicio por `HTTP/<server_host>` con usuario y contraseña. Prueba los reinos
/// candidatos (y la referencia del KDC si el usuario está en otro), en orden.
pub async fn http_ticket(
    kdc: &dyn Kdc,
    login_name: &str,
    password: &str,
    server_host: &str,
) -> Result<Ticket, KerberosError> {
    let user = match login_name.split_once('\\') {
        Some((_, u)) => Principal::user(u),
        None => Principal::user(login_name.trim()),
    };
    let mut realms = realm_candidates(login_name, server_host);
    let mut last = KerberosError::Unreachable("sin reino candidato".into());
    let mut i = 0;
    while let Some(realm) = realms.get(i).cloned() {
        i += 1;
        if i > MAX_REALMS + 1 {
            break;
        }
        match login(kdc, &realm, &user, password).await {
            Ok(tgt) => return service(kdc, &tgt, &format!("HTTP/{}", server_host.to_ascii_lowercase())).await,
            Err(KerberosError::WrongRealm(r)) => {
                let r = r.to_ascii_uppercase();
                if !realms.contains(&r) && realms.len() <= MAX_REALMS {
                    realms.insert(i, r);
                }
            }
            // Ese reino no tiene KDC alcanzable (o no es un reino): el siguiente.
            Err(e @ (KerberosError::Unreachable(_) | KerberosError::Kdc { .. } | KerberosError::Protocol(_))) => {
                last = e;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Valor de `Authorization` (`Negotiate <base64>`) para una petición con `t`, con el hash del
/// *channel binding* si se conoce el certificado del servidor.
pub fn authorization(t: &Ticket, cert: Option<&[u8]>) -> Result<String, KerberosError> {
    use base64::Engine as _;
    let cb = cert.map(crate::ntlm::channel_bindings_hash);
    let ap = ap_req(t, Some((0x8003, gss_checksum(cb))), 11, Utc::now())?;
    Ok(format!("Negotiate {}", base64::engine::general_purpose::STANDARD.encode(spnego(&ap))))
}

#[cfg(test)]
mod tests;
