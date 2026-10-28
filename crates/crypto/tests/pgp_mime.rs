//! PGP/MIME de punta a punta con claves generadas: Autocrypt, firma, cifrado, confianza
//! (Verificado / Clave nueva / No coincide), PGP en línea y WKD contra un servidor falso.

use std::sync::Arc;

use iuredesk_crypto::autocrypt::AutocryptHeader;
use iuredesk_crypto::smime::TrustStore;
use iuredesk_crypto::wkd::WkdClient;
use iuredesk_crypto::{Crypto, MemoryVault, SealRequest, SignatureState, Technology, Tone, openpgp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn crypto() -> Crypto {
    Crypto::in_memory(Arc::new(MemoryVault::default()), TrustStore::empty())
}

fn msg(from: &str, to: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: Expediente 123\r\nDate: Tue, 27 Oct 2026 10:00:00 -0600\r\nMessage-ID: <x@a>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{body}\r\n--alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>{body}</p>\r\n--alt--\r\n"
    )
    .into_bytes()
}

const SIGN: SealRequest = SealRequest { sign: true, encrypt: false, prefer_smime: false };
const BOTH: SealRequest = SealRequest { sign: true, encrypt: true, prefer_smime: false };

#[test]
fn autocrypt_then_signed_then_encrypted() {
    let ana = crypto();
    let beto = crypto();
    ana.keys.generate_pgp("Ana", "ana@despacho.mx").unwrap();
    beto.keys.generate_pgp("Beto", "beto@cliente.mx").unwrap();

    // 1) Primer correo de Ana, firmado: Beto ve «Clave nueva» y aprende su clave por Autocrypt.
    let m1 =
        ana.seal(&msg("Ana <ana@despacho.mx>", "beto@cliente.mx", "Hola"), &["beto@cliente.mx".into()], SIGN).unwrap();
    let t1 = String::from_utf8_lossy(&m1).to_string();
    assert!(t1.contains("Autocrypt: addr=ana@despacho.mx; prefer-encrypt=mutual; keydata="));
    assert!(t1.contains("multipart/signed; micalg=pgp-sha256"));
    let o1 = beto.open(&m1);
    let r1 = o1.report.clone().unwrap();
    assert_eq!(r1.technology, Technology::OpenPgp);
    assert_eq!(r1.signature.as_ref().unwrap().state, SignatureState::NewKey);
    assert_eq!(r1.title(true), "Firmado · Clave nueva");
    assert!(beto.keys.peer("ana@despacho.mx").is_some());
    // Lo que se muestra ya no es multipart/signed: es el contenido.
    let d1 = String::from_utf8_lossy(&o1.display).to_string();
    assert!(d1.contains("multipart/alternative") && !d1.contains("pgp-signature"));
    assert!(d1.starts_with("From: Ana <ana@despacho.mx>"));

    // 2) Segundo correo firmado: ahora «Verificado».
    let m2 =
        ana.seal(&msg("Ana <ana@despacho.mx>", "beto@cliente.mx", "Otra"), &["beto@cliente.mx".into()], SIGN).unwrap();
    let r2 = beto.open(&m2).report.unwrap();
    assert_eq!(r2.signature.as_ref().unwrap().state, SignatureState::Verified);
    assert_eq!(r2.tone(), Tone::Good);

    // 3) Beto responde (Ana aprende su clave) y ya pueden cifrar en los dos sentidos.
    let caps = ana.capabilities("ana@despacho.mx", &["beto@cliente.mx".into()]);
    assert!(caps.can_sign && !caps.can_encrypt && caps.missing == vec!["beto@cliente.mx".to_string()]);
    let reply =
        beto.seal(&msg("beto@cliente.mx", "ana@despacho.mx", "Recibido"), &["ana@despacho.mx".into()], SIGN).unwrap();
    ana.open(&reply);
    let caps = ana.capabilities("ana@despacho.mx", &["beto@cliente.mx".into()]);
    assert!(caps.can_encrypt && caps.all_mutual, "{caps:?}");

    let m3 = ana
        .seal(&msg("Ana <ana@despacho.mx>", "beto@cliente.mx", "Confidencial"), &["beto@cliente.mx".into()], BOTH)
        .unwrap();
    let t3 = String::from_utf8_lossy(&m3).to_string();
    assert!(t3.contains("multipart/encrypted; protocol=\"application/pgp-encrypted\""));
    assert!(!t3.contains("Confidencial"), "el texto no viaja en claro");
    assert!(t3.contains("Subject: Expediente 123"));
    let o3 = beto.open(&m3);
    let r3 = o3.report.unwrap();
    assert!(r3.encrypted);
    assert_eq!(r3.signature.unwrap().state, SignatureState::Verified);
    assert!(String::from_utf8_lossy(&o3.display).contains("Confidencial"));
    // Copia de «Enviados»: Ana la puede leer.
    let mine = ana.open(&m3);
    assert!(mine.report.unwrap().decrypt_error.is_none());

    // Un tercero no puede descifrar y lo ve claramente.
    let eve = crypto();
    eve.keys.generate_pgp("Eve", "eve@x.net").unwrap();
    let re = eve.open(&m3).report.unwrap();
    assert!(re.decrypt_error.is_some());
    assert_eq!(re.tone(), Tone::Bad);
}

#[test]
fn tampering_and_impersonation_are_flagged() {
    let ana = crypto();
    let beto = crypto();
    let mallory = crypto();
    ana.keys.generate_pgp("Ana", "ana@despacho.mx").unwrap();
    mallory.keys.generate_pgp("Ana", "ana@despacho.mx").unwrap();
    let m = ana.seal(&msg("ana@despacho.mx", "beto@cliente.mx", "Pagar 100"), &[], SIGN).unwrap();
    beto.open(&m); // aprende la clave real

    // Alterado por el camino.
    let t = String::from_utf8_lossy(&m).replace("Pagar 100", "Pagar 900");
    let st = beto.open(t.as_bytes()).report.unwrap().signature.unwrap().state;
    assert!(matches!(st, SignatureState::Invalid(_)), "{st:?}");

    // Otra clave para la misma dirección: «No coincide».
    let fake = mallory.seal(&msg("ana@despacho.mx", "beto@cliente.mx", "Pagar 900"), &[], SIGN).unwrap();
    let r = beto.open(&fake).report.unwrap();
    assert_eq!(r.signature.as_ref().unwrap().state, SignatureState::Mismatch);
    assert_eq!(r.tone(), Tone::Warning);
    // La clave importada/conocida no se reemplaza por la del atacante… (Autocrypt sí la cambia,
    // pero el mensaje quedó marcado).

    // Firma sin clave conocida ni Autocrypt: «Sin comprobar».
    let carla = crypto();
    let stripped: String = String::from_utf8_lossy(&m)
        .split("\r\n")
        .filter(|l| !l.starts_with("Autocrypt:") && !l.starts_with(' ') || l.starts_with(" protocol"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let st = carla.open(stripped.as_bytes()).report.unwrap().signature.unwrap().state;
    assert_eq!(st, SignatureState::MissingKey);
}

#[test]
fn inline_pgp_is_read_only() {
    let ana = crypto();
    let beto = crypto();
    ana.keys.generate_pgp("Ana", "ana@despacho.mx").unwrap();
    beto.keys.generate_pgp("Beto", "beto@cliente.mx").unwrap();
    beto.keys.import_public(ana.keys.export_pgp_public("ana@despacho.mx").unwrap().as_bytes()).unwrap();
    ana.keys.import_public(beto.keys.export_pgp_public("beto@cliente.mx").unwrap().as_bytes()).unwrap();

    // Mensaje cifrado en línea (como lo haría un cliente antiguo).
    let (bkey, _) = (beto.keys.public_key_for("beto@cliente.mx").unwrap(), ());
    let (akey, apw) = ana.keys.own_pgp("ana@despacho.mx").unwrap();
    let armored = openpgp::encrypt("Hola en línea".as_bytes(), &[&bkey], Some((&akey, apw.as_str()))).unwrap();
    let raw = format!(
        "From: ana@despacho.mx\r\nTo: beto@cliente.mx\r\nSubject: x\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nTexto previo\r\n{}\r\n",
        armored.replace('\n', "\r\n")
    );
    let o = beto.open(raw.as_bytes());
    let r = o.report.unwrap();
    assert!(r.inline && r.encrypted);
    assert_eq!(r.signature.unwrap().state, SignatureState::Verified);
    let d = String::from_utf8_lossy(&o.display).to_string();
    assert!(d.contains("Texto previo") && d.contains("Hola en línea") && !d.contains("BEGIN PGP"));
}

#[test]
fn autocrypt_header_from_other_sender_is_ignored() {
    let ana = crypto();
    ana.keys.generate_pgp("Ana", "ana@despacho.mx").unwrap();
    let m = ana.seal(&msg("ana@despacho.mx", "x@y.z", "hola"), &[], SealRequest::default()).unwrap();
    // Sin firmar ni cifrar: solo agrega Autocrypt.
    let t = String::from_utf8_lossy(&m).to_string();
    assert!(t.contains("Autocrypt:") && !t.contains("multipart/signed"));
    let header = t.split("Autocrypt: ").nth(1).unwrap().split("\r\nMIME-Version").next().unwrap().replace("\r\n", "");
    assert_eq!(AutocryptHeader::parse(&header).unwrap().addr, "ana@despacho.mx");
    // Un mensaje de otro remitente con el Autocrypt de Ana no le asigna la clave.
    let forged = t.replace("From: ana@despacho.mx", "From: otro@x.org");
    let beto = crypto();
    assert!(beto.open(forged.as_bytes()).report.is_none());
    assert!(beto.keys.peer("otro@x.org").is_none() && beto.keys.peer("ana@despacho.mx").is_none());
}

/// Servidor HTTP mínimo que responde una sola ruta.
async fn fake_wkd(path: String, body: Vec<u8>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..4 {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let line = req.lines().next().unwrap_or("").to_string();
            let resp = if line.contains(&path) {
                let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                    .into_bytes();
                r.extend_from_slice(&body);
                r
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            };
            let _ = s.write_all(&resp).await;
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn wkd_lookup_against_fake_server() {
    let key = openpgp::generate("Joe", "joe.doe@example.org", "x").unwrap().to_public_key();
    let (_, direct) = iuredesk_crypto::wkd::urls("Joe.Doe@example.org").unwrap();
    let path = direct.split("example.org").nth(1).unwrap().split('?').next().unwrap().to_string();
    let base = fake_wkd(path, openpgp::binary_public(&key).unwrap()).await;
    let client = WkdClient::with_base(&base).unwrap();
    let found = client.lookup("Joe.Doe@example.org").await.unwrap().unwrap();
    assert_eq!(openpgp::fingerprint(&found), openpgp::fingerprint(&key));
    // Dirección sin clave publicada.
    assert!(client.lookup("nadie@example.org").await.unwrap().is_none());
    // Y se guarda en el almacén.
    let c = crypto();
    assert!(c.wkd_fetch(&client, "joe.doe@example.org").await.unwrap());
    assert!(c.keys.peer("joe.doe@example.org").is_some());
}
