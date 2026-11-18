//! Kerberos contra un KDC falso que hace su parte de verdad: pide preautenticación con la sal de
//! AD, descifra la marca de tiempo con la contraseña verdadera, comprueba el checksum del TGS-REQ
//! y entrega tickets cifrados; el AP-REQ de HTTP se abre con la clave del servicio y se mira el
//! *channel binding*.

use std::sync::Mutex;
use std::time::Duration;

use super::der::{Tlv, children, field, fields, need, read};
use super::*;

const REALM: &str = "CORP.CONTOSO.COM";
const SALT: &str = "CORP.CONTOSO.COMana";

fn key(b: u8) -> Key {
    Key { etype: crypto::AES256, bytes: vec![b; 32] }
}

struct FakeKdc {
    password: &'static str,
    /// Reinos que «existen»; los demás no se alcanzan.
    realms: Vec<&'static str>,
    /// Reino que contesta «el usuario está en CORP.CONTOSO.COM».
    referral_from: Option<&'static str>,
    log: Mutex<Vec<String>>,
}

impl FakeKdc {
    fn new() -> Self {
        Self { password: "secreto", realms: vec![REALM], referral_from: None, log: Mutex::new(Vec::new()) }
    }

    fn error(code: i64, e_data: Option<Vec<u8>>, crealm: Option<&str>) -> Vec<u8> {
        let mut f =
            vec![ctx(0, int(5)), ctx(1, int(30)), ctx(4, gtime("20261118000000Z")), ctx(5, int(0)), ctx(6, int(code))];
        if let Some(r) = crealm {
            f.push(ctx(7, gstr(r)));
        }
        f.push(ctx(9, gstr(REALM)));
        f.push(ctx(10, Principal { kind: 2, names: vec!["krbtgt".into(), REALM.into()] }.encode()));
        if let Some(d) = e_data {
            f.push(ctx(12, octets(&d)));
        }
        app(30, seq(&f))
    }

    fn rep(tag: u8, enc_tag: u8, outer: &Key, usage: u32, session: &Key, nonce: i64, cname: &Principal) -> Vec<u8> {
        let enc = app(
            enc_tag,
            seq(&[
                ctx(0, seq(&[ctx(0, int(i64::from(session.etype))), ctx(1, octets(&session.bytes))])),
                ctx(1, seq(&[])),
                ctx(2, int(nonce)),
                ctx(4, bits32(0)),
                ctx(5, gtime("20261118000000Z")),
                ctx(7, gtime("20991231000000Z")),
                ctx(9, gstr(REALM)),
            ]),
        );
        let cipher = crypto::encrypt(outer, usage, &enc).unwrap();
        // Ticket opaco: lo que se cifró para el servicio es la clave de sesión.
        let ticket = app(
            1,
            seq(&[ctx(0, int(5)), ctx(1, gstr(REALM)), ctx(3, encrypted_data(session.etype, session.bytes.clone()))]),
        );
        app(
            tag,
            seq(&[
                ctx(0, int(5)),
                ctx(1, int(i64::from(tag & 0x1F))),
                ctx(3, gstr(REALM)),
                ctx(4, cname.encode()),
                ctx(5, ticket),
                ctx(6, encrypted_data(outer.etype, cipher)),
            ]),
        )
    }

    fn handle(&self, realm: &str, req: &[u8]) -> Result<Vec<u8>, KerberosError> {
        if !self.realms.contains(&realm) && self.referral_from != Some(realm) {
            return Err(KerberosError::Unreachable(realm.into()));
        }
        let (t, _) = read(req).unwrap();
        let f = fields(t).unwrap();
        let body = fields(need(&f, 4, "body").unwrap()).unwrap();
        let nonce = need(&body, 7, "nonce").unwrap().int().unwrap();
        let padata: Vec<Tlv<'_>> = field(&f, 3).map(|p| children(p.body).unwrap()).unwrap_or_default();
        let pa = |kind: i64| {
            padata.iter().find_map(|p| {
                let pf = fields(*p).unwrap();
                (need(&pf, 1, "t").unwrap().int().unwrap() == kind)
                    .then(|| need(&pf, 2, "v").unwrap().octets().unwrap().to_vec())
            })
        };
        let mut log = self.log.lock().unwrap();
        match t.tag {
            0x6A => {
                assert_eq!(need(&body, 2, "realm").unwrap().string().unwrap(), realm);
                let cname = Principal::decode(need(&body, 1, "cname").unwrap()).unwrap();
                if self.referral_from == Some(realm) {
                    log.push(format!("referencia {realm}"));
                    return Ok(Self::error(68, None, Some(REALM)));
                }
                assert!(pa(128).is_some(), "pide el PAC");
                let Some(ts) = pa(2) else {
                    log.push(format!("sin preautenticación {cname:?}"));
                    let info = seq(&[seq(&[ctx(0, int(18)), ctx(1, gstr(SALT))]), seq(&[ctx(0, int(23))])]);
                    return Ok(Self::error(25, Some(seq(&[pa_data(19, info)])), None));
                };
                let user = crypto::string_to_key(18, self.password, SALT.as_bytes(), None).unwrap();
                let (enc, _) = read(&ts).unwrap();
                let ef = fields(enc).unwrap();
                match crypto::decrypt(&user, 1, need(&ef, 2, "c").unwrap().octets().unwrap()) {
                    Ok(plain) => {
                        let (ts, _) = read(&plain).unwrap();
                        let when = need(&fields(ts).unwrap(), 0, "t").unwrap().string().unwrap();
                        assert!(parse_ktime(&when).is_some(), "{when}");
                    }
                    Err(_) => {
                        log.push("contraseña equivocada".into());
                        return Ok(Self::error(24, None, None));
                    }
                }
                log.push(format!("TGT para {cname:?}"));
                // El KDC canonicaliza el enterprise principal a la cuenta.
                let canonical = Principal { kind: 1, names: vec!["ana".into()] };
                Ok(Self::rep(11, 25, &user, 3, &key(7), nonce, &canonical))
            }
            0x6C => {
                // PA-TGS-REQ: AP-REQ con el TGT; el autenticador va con la clave de sesión del TGT.
                let ap = pa(1).expect("PA-TGS-REQ");
                let (apt, _) = read(&ap).unwrap();
                let af = fields(apt).unwrap();
                let (enc, _) = read(need(&af, 4, "authenticator").unwrap().raw).unwrap();
                let auth = crypto::decrypt(&key(7), 7, need(&fields(enc).unwrap(), 2, "c").unwrap().octets().unwrap())
                    .unwrap();
                let (at, _) = read(&auth).unwrap();
                let atf = fields(at).unwrap();
                assert_eq!(need(&atf, 1, "crealm").unwrap().string().unwrap(), REALM);
                let ck = fields(need(&atf, 3, "cksum").unwrap()).unwrap();
                assert_eq!(need(&ck, 0, "tipo").unwrap().int().unwrap(), 16);
                let raw_body = need(&f, 4, "body").unwrap().raw;
                assert_eq!(
                    need(&ck, 1, "valor").unwrap().octets().unwrap(),
                    crypto::checksum(&key(7), 6, raw_body).unwrap()
                );
                let sname = Principal::decode(need(&body, 3, "sname").unwrap()).unwrap();
                log.push(format!("servicio {}", sname.names.join("/")));
                if !matches!(sname.names.get(1).map(String::as_str), Some("mail.corp.contoso.com" | "127.0.0.1")) {
                    return Ok(Self::error(7, None, None));
                }
                let cname = Principal { kind: 1, names: vec!["ana".into()] };
                Ok(Self::rep(13, 26, &key(7), 8, &key(9), nonce, &cname))
            }
            other => panic!("petición inesperada {other:#x}"),
        }
    }
}

impl Kdc for FakeKdc {
    fn exchange<'a>(&'a self, realm: &'a str, req: Vec<u8>) -> BoxFuture<'a, Result<Vec<u8>, KerberosError>> {
        Box::pin(async move { self.handle(realm, &req) })
    }
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

#[test]
fn upn_login_gets_an_http_ticket_and_the_ap_req_opens_with_the_service_key() {
    let kdc = FakeKdc::new();
    let t = block(http_ticket(&kdc, "ana@corp.contoso.com", "secreto", "mail.corp.contoso.com")).unwrap();
    assert_eq!(t.crealm, REALM);
    assert_eq!(t.cname.names, ["ana"]);
    assert_eq!(t.key, key(9));
    assert!(t.fresh(Utc::now()));
    let log = kdc.log.lock().unwrap().clone();
    assert_eq!(
        log.first().map(String::as_str),
        Some("sin preautenticación Principal { kind: 10, names: [\"ana@corp.contoso.com\"] }")
    );
    assert_eq!(log.last().map(String::as_str), Some("servicio HTTP/mail.corp.contoso.com"));

    // Negotiate: SPNEGO → token GSS de Kerberos → AP-REQ → autenticador (uso 11) con el
    // checksum GSS que trae el channel binding del certificado.
    use base64::Engine as _;
    let header = authorization(&t, Some(b"certificado")).unwrap();
    let token = base64::engine::general_purpose::STANDARD.decode(header.strip_prefix("Negotiate ").unwrap()).unwrap();
    let (outer, _) = read(&token).unwrap();
    assert_eq!(outer.tag, 0x60);
    let (oid, rest) = read(outer.body).unwrap();
    assert_eq!(oid.body, OID_SPNEGO);
    let (init, _) = read(rest).unwrap();
    let init = fields(read(init.body).unwrap().0).unwrap();
    let gss = need(&init, 2, "mechToken").unwrap().octets().unwrap();
    let (g, _) = read(gss).unwrap();
    let (koid, rest) = read(g.body).unwrap();
    assert_eq!(koid.body, OID_KRB5);
    assert_eq!(&rest[..2], &[1, 0]);
    let (ap, _) = read(&rest[2..]).unwrap();
    assert_eq!(ap.tag, 0x6E);
    let af = fields(ap).unwrap();
    assert_eq!(need(&af, 3, "ticket").unwrap().raw, &t.ticket[..]);
    let enc = fields(need(&af, 4, "auth").unwrap()).unwrap();
    let auth = crypto::decrypt(&key(9), 11, need(&enc, 2, "c").unwrap().octets().unwrap()).unwrap();
    let atf = fields(read(&auth).unwrap().0).unwrap();
    let ck = fields(need(&atf, 3, "cksum").unwrap()).unwrap();
    assert_eq!(need(&ck, 0, "tipo").unwrap().int().unwrap(), 0x8003);
    let v = need(&ck, 1, "valor").unwrap().octets().unwrap();
    assert_eq!(&v[..4], &16u32.to_le_bytes());
    assert_eq!(&v[4..20], &crate::ntlm::channel_bindings_hash(b"certificado"));
}

#[test]
fn domain_backslash_user_and_referral_and_unreachable_realms() {
    // CONTOSO.COM (padre del servidor) remite a CORP.CONTOSO.COM; un reino sin KDC se salta.
    let kdc = FakeKdc { realms: vec![REALM], referral_from: Some("CONTOSO.COM"), ..FakeKdc::new() };
    let t = block(http_ticket(&kdc, "contoso.com\\ana", "secreto", "mail.corp.contoso.com")).unwrap();
    assert_eq!(t.crealm, REALM);
    let log = kdc.log.lock().unwrap().clone();
    assert_eq!(log.first().map(String::as_str), Some("referencia CONTOSO.COM"));
    assert!(log.iter().any(|l| l.contains("kind: 1, names: [\"ana\"]")), "{log:?}");
}

#[test]
fn wrong_password_unknown_service_and_no_kdc() {
    let kdc = FakeKdc { password: "otra", ..FakeKdc::new() };
    assert_eq!(
        block(http_ticket(&kdc, "ana@corp.contoso.com", "secreto", "mail.corp.contoso.com")),
        Err(KerberosError::BadPassword)
    );
    // Una sola contraseña equivocada: no se prueba en otros reinos (bloqueo de cuenta).
    assert_eq!(kdc.log.lock().unwrap().iter().filter(|l| l.contains("equivocada")).count(), 1);

    let kdc = FakeKdc::new();
    assert_eq!(
        block(http_ticket(&kdc, "ana@corp.contoso.com", "secreto", "web.corp.contoso.com")),
        Err(KerberosError::NoService("HTTP/web.corp.contoso.com".into()))
    );

    let kdc = FakeKdc { realms: vec![], ..FakeKdc::new() };
    assert!(matches!(
        block(http_ticket(&kdc, "ana@contoso.com", "secreto", "mail.contoso.com")),
        Err(KerberosError::Unreachable(_))
    ));
}

#[test]
fn realm_candidates_from_login_and_server() {
    assert_eq!(realm_candidates("ana@contoso.com", "mail.corp.contoso.com"), ["CONTOSO.COM", "CORP.CONTOSO.COM"]);
    assert_eq!(realm_candidates("CONTOSO\\ana", "mail.contoso.com"), ["CONTOSO.COM"]);
    assert_eq!(realm_candidates("ana", "10.0.0.5"), Vec::<String>::new());
    assert!(realm_candidates("ana@x.y", "a.b.c.d.e.f.g.h.example.com").len() <= MAX_REALMS);
}

#[test]
fn hostile_replies_are_errors() {
    for b in [&b""[..], b"\x6b\x00", b"\x7e\x03\x30\x01\x00", b"\x30\x00", &[0x6b, 0x84, 0xff, 0xff, 0xff, 0xff]] {
        assert!(parse_reply(b).is_err(), "{b:?}");
    }
    assert!(parse_enc_rep(&app(25, seq(&[ctx(0, seq(&[ctx(0, int(18)), ctx(1, octets(&[1; 5]))]))]))).is_err());
    assert!(etype_info2(b"\x30\x03\x02\x01\x05").is_empty());
    assert!(etype_info2(&[0xff; 40]).is_empty());
}

#[test]
fn principal_and_time_round_trip() {
    let p = Principal::user("ana@contoso.com");
    assert_eq!(p.kind, NT_ENTERPRISE);
    assert_eq!(Principal::decode(read(&p.encode()).unwrap().0).unwrap(), p);
    let now = parse_ktime("20261118093000Z").unwrap();
    assert_eq!(ktime(now), "20261118093000Z");
}

#[path = "../../../dav/tests/common/mod.rs"]
mod common;

/// El autenticador (en claro) de un `Authorization: Negotiate …` de Kerberos, abierto con la clave
/// del servicio; `None` si no es Kerberos (p. ej. NTLM dentro de Negotiate) o no abre.
fn open_negotiate(header: &str, service_key: &Key) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let token = base64::engine::general_purpose::STANDARD.decode(header.strip_prefix("Negotiate ")?).ok()?;
    let (outer, _) = read(&token).ok()?;
    let (_, rest) = read(outer.body).ok()?;
    let (init, _) = read(rest).ok()?;
    let init = fields(read(init.body).ok()?.0).ok()?;
    let gss = field(&init, 2)?.octets().ok()?;
    let (g, _) = read(gss).ok()?;
    let (_, rest) = read(g.body).ok()?;
    let (ap, _) = read(rest.get(2..)?).ok()?;
    let af = fields(ap).ok()?;
    let enc = fields(field(&af, 4)?).ok()?;
    crypto::decrypt(service_key, 11, field(&enc, 2)?.octets().ok()?).ok()
}

/// Servidor que solo ofrece `Negotiate` y solo acepta Kerberos (NTLM restringido en el dominio):
/// el cliente pide el ticket al KDC y cada petición lleva un AP-REQ nuevo; la segunda ya no pasa
/// por el KDC. Contraseña equivocada: «credencial rechazada» sin probar NTLM.
#[test]
fn ews_client_uses_kerberos_when_only_negotiate_is_offered() {
    use crate::{BasicAuth, EwsClient, EwsError};
    use common::{Resp, serve};
    use std::sync::Arc;
    block(async {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let l = Arc::clone(&log);
        let base = serve(move |req| {
            let auth = req.header("Authorization").unwrap_or_default().to_string();
            let mut l = l.lock().unwrap();
            match open_negotiate(&auth, &key(9)) {
                Some(_) => {
                    l.push("kerberos".into());
                    Resp::xml(
                        200,
                        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:SyncFolderHierarchyResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>K1</m:SyncState><m:IncludesLastFolderInRange>true</m:IncludesLastFolderInRange><m:Changes/></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse></s:Body></s:Envelope>"#,
                    )
                }
                None => {
                    l.push(format!("rechazo {}", auth.split(' ').next().unwrap_or("")));
                    Resp::new(401, "").with("WWW-Authenticate", "Negotiate")
                }
            }
        })
        .await;
        // El servidor de pruebas es 127.0.0.1: el reino sale del UPN y el servicio debe ser el
        // que el KDC falso conoce, así que se le acepta cualquiera.
        let url = format!("{base}/EWS/Exchange.asmx");
        let kdc = Arc::new(FakeKdc::new());
        let client = |pw: &str, kdc: Arc<FakeKdc>| {
            EwsClient::new(&url, Arc::new(BasicAuth::new("ana@corp.contoso.com", pw)), Duration::from_secs(10))
                .unwrap()
                .with_kdc(kdc)
        };
        let c = client("secreto", Arc::clone(&kdc));
        let h = c.sync_folder_hierarchy(None).await.unwrap();
        assert_eq!(h.state, "K1");
        c.sync_folder_hierarchy(None).await.unwrap();
        {
            let l = log.lock().unwrap();
            assert_eq!(l.iter().filter(|x| *x == "kerberos").count(), 2, "{l:?}");
            assert!(!l.iter().any(|x| x == "rechazo Negotiate"), "no se probó NTLM: {l:?}");
            let k = kdc.log.lock().unwrap();
            assert_eq!(k.iter().filter(|x| x.starts_with("servicio")).count(), 1, "un solo ticket: {k:?}");
        }
        let bad = Arc::new(FakeKdc { password: "otra", ..FakeKdc::new() });
        let err = client("secreto", bad).sync_folder_hierarchy(None).await.unwrap_err();
        assert_eq!(err, EwsError::Unauthorized);
        assert!(!log.lock().unwrap().iter().any(|x| x == "rechazo Negotiate"));
    });
}
