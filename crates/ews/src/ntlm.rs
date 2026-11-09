//! NTLMv2 para HTTP (MS-NLMP), propio y en Rust puro: lo que pide Exchange Server local cuando
//! la autenticación básica está apagada (F8.2, hito 3).
//!
//! - `NEGOTIATE` → `CHALLENGE` (del servidor, leído con límites) → `AUTHENTICATE` con respuesta
//!   NTLMv2, `MsvAvTimestamp` del servidor, **MIC** y, sobre TLS, **channel binding**
//!   (`MsvAvChannelBindings` = MD5 de `tls-server-end-point` del certificado del servidor), que
//!   exige la *Extended Protection* de Exchange 2019 CU14 y posteriores.
//! - Sin `NEGOTIATE_KEY_EXCH` (no hace falta RC4: HTTP no firma ni sella).
//! - El usuario se escribe como `DOMINIO\usuario` o como UPN (`ana@contoso.com`, dominio vacío).

use hmac::{Hmac, Mac};
use md4::{Digest as _, Md4};
use md5::Md5;

/// Fallo de NTLM.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NtlmError {
    #[error("desafío NTLM no válido: {0}")]
    BadChallenge(&'static str),
    #[error("azar del sistema: {0}")]
    Random(String),
}

const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";
/// UNICODE | REQUEST_TARGET | NTLM | ALWAYS_SIGN | EXTENDED_SESSIONSECURITY | TARGET_INFO |
/// VERSION | 128 | 56.
const FLAGS: u32 = 0x0000_0001
    | 0x0000_0004
    | 0x0000_0200
    | 0x0000_8000
    | 0x0008_0000
    | 0x0080_0000
    | 0x0200_0000
    | 0x2000_0000
    | 0x8000_0000;
/// Versión que se anuncia (6.1, compilación 7601, revisión NTLMSSP 15): solo informativa.
const VERSION: [u8; 8] = [6, 1, 0xB1, 0x1D, 0, 0, 0, 15];
/// Tamaño máximo de un desafío que se acepta.
const MAX_CHALLENGE: usize = 64 * 1024;

const AV_EOL: u16 = 0;
const AV_FLAGS: u16 = 6;
const AV_TIMESTAMP: u16 = 7;
const AV_TARGET_NAME: u16 = 9;
const AV_CHANNEL_BINDINGS: u16 = 10;

/// Credenciales de dominio.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub domain: String,
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("domain", &self.domain).field("user", &self.user).finish_non_exhaustive()
    }
}

impl Credentials {
    /// `DOMINIO\usuario` → (dominio, usuario); cualquier otra forma (UPN) va con dominio vacío.
    pub fn from_login(login: &str, password: &str) -> Self {
        let login = login.trim();
        let (domain, user) = match login.split_once('\\') {
            Some((d, u)) => (d.to_string(), u.to_string()),
            None => (String::new(), login.to_string()),
        };
        Self { domain, user, password: password.to_string() }
    }
}

/// El desafío del servidor (`CHALLENGE_MESSAGE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    pub flags: u32,
    pub server_challenge: [u8; 8],
    /// `TargetInfo` tal cual (pares AV).
    pub target_info: Vec<u8>,
    /// El mensaje entero (entra en el MIC).
    pub raw: Vec<u8>,
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i.checked_add(1)?)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    let s = b.get(i..i.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Un campo `(len, maxlen, offset)` de un mensaje NTLM.
fn field(b: &[u8], at: usize) -> Option<&[u8]> {
    let len = usize::from(u16_at(b, at)?);
    let off = usize::try_from(u32_at(b, at.checked_add(4)?)?).ok()?;
    b.get(off..off.checked_add(len)?)
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let Ok(mut m) = <Hmac<Md5> as Mac>::new_from_slice(key) else { return [0; 16] };
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// Mensaje `NEGOTIATE` (sin dominio ni equipo: los da el `AUTHENTICATE`).
pub fn negotiate() -> Vec<u8> {
    let mut m = Vec::with_capacity(40);
    m.extend_from_slice(SIGNATURE);
    m.extend_from_slice(&1u32.to_le_bytes());
    m.extend_from_slice(&FLAGS.to_le_bytes());
    m.extend_from_slice(&[0u8; 8]); // dominio
    m.extend_from_slice(&[0u8; 8]); // equipo
    m.extend_from_slice(&VERSION);
    m
}

/// Lee el `CHALLENGE` del servidor con todos los límites.
pub fn parse_challenge(b: &[u8]) -> Result<Challenge, NtlmError> {
    if b.len() > MAX_CHALLENGE {
        return Err(NtlmError::BadChallenge("demasiado grande"));
    }
    if b.get(..8) != Some(SIGNATURE.as_slice()) || u32_at(b, 8) != Some(2) {
        return Err(NtlmError::BadChallenge("no es un CHALLENGE"));
    }
    let flags = u32_at(b, 20).ok_or(NtlmError::BadChallenge("corto"))?;
    let mut server_challenge = [0u8; 8];
    server_challenge.copy_from_slice(b.get(24..32).ok_or(NtlmError::BadChallenge("corto"))?);
    // TargetInfo (si el servidor la manda; sin ella no hay NTLMv2 con marca de tiempo).
    let target_info = if b.len() >= 48 { field(b, 40).map(<[u8]>::to_vec).unwrap_or_default() } else { Vec::new() };
    Ok(Challenge { flags, server_challenge, target_info, raw: b.to_vec() })
}

/// Pares AV `(id, valor)` hasta `MsvAvEOL`.
fn av_pairs(info: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while out.len() < 64 {
        let (Some(id), Some(len)) = (u16_at(info, i), u16_at(info, i.saturating_add(2))) else { break };
        if id == AV_EOL {
            break;
        }
        let start = i.saturating_add(4);
        let Some(v) = info.get(start..start.saturating_add(usize::from(len))) else { break };
        out.push((id, v));
        i = start.saturating_add(usize::from(len));
    }
    out
}

fn push_av(out: &mut Vec<u8>, id: u16, v: &[u8]) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&u16::try_from(v.len()).unwrap_or(u16::MAX).to_le_bytes());
    out.extend_from_slice(v.get(..usize::from(u16::MAX).min(v.len())).unwrap_or_default());
}

/// `NTOWFv2` (MS-NLMP 3.3.2).
pub fn ntowf_v2(c: &Credentials) -> [u8; 16] {
    let nt = Md4::digest(utf16(&c.password));
    hmac_md5(&nt, &[&utf16(&format!("{}{}", c.user.to_uppercase(), c.domain))])
}

/// `tls-server-end-point` (RFC 5929) de un certificado DER: su hash con el algoritmo de su firma
/// (SHA-256 si es MD5/SHA-1 o no se reconoce).
pub fn tls_server_end_point(cert_der: &[u8]) -> Vec<u8> {
    use sha2::Digest as _;
    let has = |oid: &[u8]| cert_der.windows(oid.len()).any(|w| w == oid);
    // sha384WithRSA, ecdsa-with-SHA384 / sha512WithRSA, ecdsa-with-SHA512.
    if has(&[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C])
        || has(&[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03])
    {
        sha2::Sha384::digest(cert_der).to_vec()
    } else if has(&[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D])
        || has(&[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x04])
    {
        sha2::Sha512::digest(cert_der).to_vec()
    } else {
        sha2::Sha256::digest(cert_der).to_vec()
    }
}

/// `MsvAvChannelBindings`: MD5 de la estructura `gss_channel_bindings` con
/// `tls-server-end-point:<hash>` como datos de aplicación.
pub fn channel_bindings_hash(cert_der: &[u8]) -> [u8; 16] {
    let mut app = b"tls-server-end-point:".to_vec();
    app.extend_from_slice(&tls_server_end_point(cert_der));
    let mut s = vec![0u8; 16]; // direcciones del iniciador y del aceptante: vacías
    s.extend_from_slice(&u32::try_from(app.len()).unwrap_or(0).to_le_bytes());
    s.extend_from_slice(&app);
    Md5::digest(&s).into()
}

/// Lo que el cliente pone de su parte (fijo en las pruebas).
#[derive(Debug, Clone, Copy)]
pub struct ClientNonce {
    pub client_challenge: [u8; 8],
    /// FILETIME (cientos de ns desde 1601) si el servidor no manda `MsvAvTimestamp`.
    pub now_filetime: u64,
}

impl ClientNonce {
    pub fn random() -> Result<Self, NtlmError> {
        let mut c = [0u8; 8];
        getrandom::getrandom(&mut c).map_err(|e| NtlmError::Random(e.to_string()))?;
        let unix_100ns =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() / 100).unwrap_or(0);
        let filetime = u64::try_from(unix_100ns).unwrap_or(0).saturating_add(116_444_736_000_000_000);
        Ok(Self { client_challenge: c, now_filetime: filetime })
    }
}

/// Respuesta NTLMv2 (MS-NLMP 3.3.2): (NtChallengeResponse, LmChallengeResponse, SessionBaseKey).
pub fn v2_responses(
    key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
    timestamp: &[u8; 8],
    target_info: &[u8],
    lm_zero: bool,
) -> (Vec<u8>, Vec<u8>, [u8; 16]) {
    let mut temp = vec![1u8, 1, 0, 0, 0, 0, 0, 0];
    temp.extend_from_slice(timestamp);
    temp.extend_from_slice(client_challenge);
    temp.extend_from_slice(&[0u8; 4]);
    temp.extend_from_slice(target_info);
    temp.extend_from_slice(&[0u8; 4]);
    let proof = hmac_md5(key, &[server_challenge, &temp]);
    let mut nt = proof.to_vec();
    nt.extend_from_slice(&temp);
    let lm = if lm_zero {
        vec![0u8; 24]
    } else {
        let mut lm = hmac_md5(key, &[server_challenge, client_challenge]).to_vec();
        lm.extend_from_slice(client_challenge);
        lm
    };
    (nt, lm, hmac_md5(key, &[&proof]))
}

/// Mensaje `AUTHENTICATE`. `negotiate` es el que se mandó (entra en el MIC); `cert_der`, el
/// certificado TLS del servidor (channel binding); `spn`, `HTTP/servidor`.
pub fn authenticate(
    creds: &Credentials,
    negotiate: &[u8],
    challenge: &Challenge,
    cert_der: Option<&[u8]>,
    spn: &str,
    nonce: ClientNonce,
) -> Vec<u8> {
    let key = ntowf_v2(creds);
    // TargetInfo del cliente: la del servidor + MsvAvFlags (hay MIC) + nombre del servicio +
    // channel binding, y fin.
    let pairs = av_pairs(&challenge.target_info);
    let server_ts = pairs.iter().find(|(id, v)| *id == AV_TIMESTAMP && v.len() == 8).map(|(_, v)| *v);
    let mut info = Vec::new();
    let mut flags_value = 0u32;
    for (id, v) in &pairs {
        match *id {
            AV_FLAGS => flags_value = v.get(..4).and_then(|f| u32_at(f, 0)).unwrap_or(0),
            AV_TARGET_NAME | AV_CHANNEL_BINDINGS => {}
            _ => push_av(&mut info, *id, v),
        }
    }
    let with_mic = server_ts.is_some();
    if with_mic {
        flags_value |= 0x2;
    }
    if flags_value != 0 {
        push_av(&mut info, AV_FLAGS, &flags_value.to_le_bytes());
    }
    push_av(&mut info, AV_TARGET_NAME, &utf16(spn));
    push_av(&mut info, AV_CHANNEL_BINDINGS, &cert_der.map(channel_bindings_hash).unwrap_or([0u8; 16]));
    push_av(&mut info, AV_EOL, &[]);

    let mut timestamp = [0u8; 8];
    match server_ts {
        Some(ts) => timestamp.copy_from_slice(ts),
        None => timestamp = nonce.now_filetime.to_le_bytes(),
    }
    let (nt, lm, session_key) =
        v2_responses(&key, &challenge.server_challenge, &nonce.client_challenge, &timestamp, &info, with_mic);

    let domain = utf16(&creds.domain);
    let user = utf16(&creds.user);
    let workstation: Vec<u8> = Vec::new();
    let flags = FLAGS & (challenge.flags | 0x0000_0001);
    const HEADER: usize = 88;
    let mut payload: Vec<u8> = Vec::new();
    let mut fields: Vec<u8> = Vec::new();
    for part in [&lm, &nt, &domain, &user, &workstation, &Vec::new()] {
        let off = HEADER.saturating_add(payload.len());
        let len = u16::try_from(part.len()).unwrap_or(0);
        fields.extend_from_slice(&len.to_le_bytes());
        fields.extend_from_slice(&len.to_le_bytes());
        fields.extend_from_slice(&u32::try_from(off).unwrap_or(0).to_le_bytes());
        payload.extend_from_slice(part.get(..usize::from(len)).unwrap_or_default());
    }
    let mut m = Vec::with_capacity(HEADER.saturating_add(payload.len()));
    m.extend_from_slice(SIGNATURE);
    m.extend_from_slice(&3u32.to_le_bytes());
    m.extend_from_slice(&fields);
    m.extend_from_slice(&flags.to_le_bytes());
    m.extend_from_slice(&VERSION);
    m.extend_from_slice(&[0u8; 16]); // MIC (se calcula con él en ceros)
    m.extend_from_slice(&payload);
    if with_mic {
        let mic = hmac_md5(&session_key, &[negotiate, &challenge.raw, &m]);
        if let Some(slot) = m.get_mut(72..88) {
            slot.copy_from_slice(&mic);
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    fn spec_creds() -> Credentials {
        Credentials { domain: "Domain".into(), user: "User".into(), password: "Password".into() }
    }

    /// Vectores de MS-NLMP §4.2.4 (NTLMv2).
    #[test]
    fn ms_nlmp_v2_vectors() {
        assert_eq!(Md4::digest(utf16("Password")).to_vec(), hex("a4 f4 9c 40 65 10 bd ca b6 82 4e e7 c3 0f d8 52"));
        let key = ntowf_v2(&spec_creds());
        assert_eq!(key.to_vec(), hex("0c 86 8a 40 3b fd 7a 93 a3 00 1e f2 2e f0 2e 3f"));
        let server: [u8; 8] = hex("01 23 45 67 89 ab cd ef").try_into().unwrap();
        let client = [0xAAu8; 8];
        let info = hex(
            "02 00 0c 00 44 00 6f 00 6d 00 61 00 69 00 6e 00 01 00 0c 00 53 00 65 00 72 00 76 00 65 00 72 00 00 00 00 00",
        );
        let (nt, lm, session) = v2_responses(&key, &server, &client, &[0u8; 8], &info, false);
        assert_eq!(nt[..16].to_vec(), hex("68 cd 0a b8 51 e5 1c 96 aa bc 92 7b eb ef 6a 1c"));
        assert_eq!(lm, hex("86 c3 50 97 ac 9c ec 10 25 54 76 4a 57 cc cc 19 aa aa aa aa aa aa aa aa"));
        assert_eq!(session.to_vec(), hex("8d e4 0c ca db c1 4a 82 f1 5c b0 ad 0d e9 5c a3"));
    }

    fn challenge_with(info: &[u8]) -> Vec<u8> {
        let mut c = SIGNATURE.to_vec();
        c.extend_from_slice(&2u32.to_le_bytes());
        c.extend_from_slice(&[0, 0, 0, 0, 48, 0, 0, 0]); // nombre de destino vacío
        c.extend_from_slice(&0xA289_8205u32.to_le_bytes());
        c.extend_from_slice(&hex("01 23 45 67 89 ab cd ef"));
        c.extend_from_slice(&[0u8; 8]);
        let len = u16::try_from(info.len()).unwrap();
        c.extend_from_slice(&len.to_le_bytes());
        c.extend_from_slice(&len.to_le_bytes());
        c.extend_from_slice(&56u32.to_le_bytes());
        c.extend_from_slice(&VERSION);
        c.extend_from_slice(info);
        c
    }

    #[test]
    fn authenticate_message_layout_and_mic() {
        let mut info = Vec::new();
        push_av(&mut info, 2, &utf16("CONTOSO"));
        push_av(&mut info, AV_TIMESTAMP, &[1, 2, 3, 4, 5, 6, 7, 8]);
        push_av(&mut info, AV_EOL, &[]);
        let raw = challenge_with(&info);
        let ch = parse_challenge(&raw).unwrap();
        assert_eq!(ch.server_challenge.to_vec(), hex("01 23 45 67 89 ab cd ef"));
        assert_eq!(ch.target_info, info);
        let creds = Credentials::from_login("CONTOSO\\ana", "secreto");
        assert_eq!((creds.domain.as_str(), creds.user.as_str()), ("CONTOSO", "ana"));
        let neg = negotiate();
        let nonce = ClientNonce { client_challenge: [7; 8], now_filetime: 0 };
        let m = authenticate(&creds, &neg, &ch, Some(b"certificado"), "HTTP/mail.contoso.com", nonce);
        assert_eq!(&m[..8], SIGNATURE);
        assert_eq!(u32_at(&m, 8), Some(3));
        // LM en ceros (hay marca de tiempo); NT = prueba + estructura con la marca del servidor.
        assert_eq!(field(&m, 12).unwrap(), &[0u8; 24]);
        let nt = field(&m, 20).unwrap();
        assert_eq!(&nt[24..32], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(field(&m, 28).unwrap(), utf16("CONTOSO").as_slice());
        assert_eq!(field(&m, 36).unwrap(), utf16("ana").as_slice());
        // TargetInfo del cliente: MsvAvFlags con MIC, nombre del servicio y channel binding.
        let client_info = &nt[44..nt.len() - 4];
        let pairs = av_pairs(client_info);
        assert!(pairs.iter().any(|(id, v)| *id == AV_FLAGS && *v == 2u32.to_le_bytes()));
        assert!(pairs.iter().any(|(id, v)| *id == AV_TARGET_NAME && *v == utf16("HTTP/mail.contoso.com").as_slice()));
        assert!(pairs.iter().any(|(id, v)| *id == AV_CHANNEL_BINDINGS && *v == channel_bindings_hash(b"certificado")));
        // El MIC se recalcula igual con su hueco en ceros.
        let key = ntowf_v2(&creds);
        let session = hmac_md5(&key, &[&nt[..16]]);
        let mut zeroed = m.clone();
        zeroed[72..88].copy_from_slice(&[0; 16]);
        assert_eq!(m[72..88].to_vec(), hmac_md5(&session, &[&neg, &raw, &zeroed]).to_vec());
        // UPN: dominio vacío.
        let upn = Credentials::from_login(" ana@contoso.com ", "x");
        assert_eq!((upn.domain.as_str(), upn.user.as_str()), ("", "ana@contoso.com"));
        assert!(!format!("{upn:?}").contains("\"x\""));
    }

    #[test]
    fn hostile_challenges_are_rejected() {
        assert!(parse_challenge(b"").is_err());
        assert!(parse_challenge(b"NTLMSSP\0\x01\0\0\0").is_err(), "no es un desafío");
        assert!(parse_challenge(b"NTLMSSP\0\x02\0\0\0short").is_err());
        // TargetInfo que apunta fuera: se ignora, no se lee de más.
        let mut c = challenge_with(&[]);
        c[44..48].copy_from_slice(&u32::MAX.to_le_bytes());
        c[40..42].copy_from_slice(&500u16.to_le_bytes());
        assert!(parse_challenge(&c).unwrap().target_info.is_empty());
        assert!(av_pairs(&[7, 0, 0xFF, 0xFF, 1, 2]).is_empty(), "par AV más largo que los datos");
    }

    #[test]
    fn end_point_hash_follows_the_signature() {
        let sha384_rsa = [0x2Au8, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C];
        assert_eq!(tls_server_end_point(&sha384_rsa).len(), 48);
        assert_eq!(tls_server_end_point(b"sha256 o desconocido").len(), 32);
    }
}
