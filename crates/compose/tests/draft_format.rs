//! Regresión del dueño (2026-11-24): un borrador de respuesta, guardado sin destinatarios (para que
//! no saliera por error), se reabría como texto plano: líneas cortadas, `\-`, `C\_LIQUIDA`, sin
//! negritas ni código, y sin los resaltados amarillo y verde. Y los resaltados del editor tampoco
//! llegaban en el correo enviado.

use iuredesk_compose::*;
use iuredesk_model::Mailbox;
use iuredesk_model::account::Identity;

const MD: &str = "Lo revisé, pero **no resuelve**: <mark style=\"background-color: #FFFF00\">siguen faltando las tablas de `DEPF`.</mark>\n\n- `sab.df`: 219 tablas.\n- `C_LIQUIDA` es texto.\n\n<mark style=\"background-color: #00FF00\">**Para pedirle al equipo:**</mark> y <span style=\"color: #FF0000\">urgente</span>.\n\n1. El listado de `GEDROPE`.\n2. El resultado de la consulta.\n";

fn ident() -> Identity {
    Identity { name: "Eduardo".into(), email: "ellaguno@confiabilidad.com.mx".into(), signature: None, reply_to: None }
}

fn draft(to: bool) -> Draft {
    let mut d = Draft::new(ident());
    if to {
        d.to = vec![Mailbox::new("Dennys", "dcervantes@confiabilidad.com.mx")];
    }
    d.subject = "Re: Proyecto".into();
    d.body = RichBody::from_markdown(MD);
    d
}

#[test]
fn a_draft_without_recipients_keeps_its_format() {
    let opts = BuildOptions { keep_bcc: true, ..BuildOptions::now() };
    let built = build_message(&draft(false), &FsResolver, &opts).expect("un borrador se guarda sin destinatarios");
    let back = reopen_draft(&built.raw, std::slice::from_ref(&ident())).unwrap();
    let md = back.body.to_markdown();
    for want in [
        "**no resuelve**",
        "<mark style=\"background-color: #FFFF00\">siguen faltando las tablas de `DEPF`.</mark>",
        "- `C_LIQUIDA` es texto.",
        "<mark style=\"background-color: #00FF00\">**Para pedirle al equipo:**</mark>",
        "<span style=\"color: #FF0000\">urgente</span>",
        "1. El listado de `GEDROPE`.\n2. El resultado de la consulta.",
    ] {
        assert!(md.contains(want), "falta «{want}» en:\n{md}");
    }
    assert!(!md.contains("\\-") && !md.contains("C\\_"), "sin escapes de texto plano:\n{md}");
    // Al enviar sí hacen falta destinatarios.
    assert!(matches!(build_message(&draft(false), &FsResolver, &BuildOptions::now()), Err(ComposeError::NoRecipients)));
}

#[test]
fn colors_and_highlights_reach_the_sent_email() {
    let built = build_message(&draft(true), &FsResolver, &BuildOptions::now()).unwrap();
    let raw = String::from_utf8_lossy(&built.raw).to_string();
    // El HTML va en quoted-printable: se busca sin los saltos suaves.
    let flat = raw.replace("=\r\n", "").replace("=3D", "=");
    assert!(flat.contains("background-color:#FFFF00"), "resaltado amarillo");
    assert!(flat.contains("background-color:#00FF00"), "resaltado verde");
    assert!(flat.contains("color:#FF0000"), "color de letra");
}
