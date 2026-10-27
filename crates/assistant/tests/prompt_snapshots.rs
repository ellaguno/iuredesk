//! Instantáneas de lo que se envía en cada tarea e idioma (`tests/snapshots/<tarea>.<idioma>.txt`).
//! Para regenerarlas tras cambiar una instrucción: `UPDATE_SNAPSHOTS=1 cargo test -p iuredesk-assistant`.

use std::path::PathBuf;

use chrono::NaiveDate;
use iuredesk_assistant::tasks::{self, Length, Target, TaskOptions, Tone};
use iuredesk_assistant::{Addr, EmailInput, Lang};

fn thread() -> Vec<EmailInput> {
    vec![
        EmailInput {
            id: "msg-1".into(),
            from: Addr::new(Some("Ana Ruiz"), "ana@cliente.mx"),
            to: vec![Addr::new(Some("Luis Pérez"), "luis@despacho.mx")],
            cc: vec![Addr::new(None, "socios@despacho.mx")],
            subject: "Contrato de arrendamiento".into(),
            date: Some("2026-10-24T17:05:00Z".into()),
            body_text: "Hola Luis:\n\n¿Podemos vernos el martes a las 10 para revisar el contrato? \
                        El depósito sería de $24,000 MXN. Mi celular es 55 1234 5678."
                .into(),
            attachments: vec!["contrato-v2.pdf".into()],
            unread: true,
        },
        EmailInput {
            id: "msg-2".into(),
            from: Addr::new(Some("Boletín"), "news@proveedor.com"),
            to: vec![Addr::new(None, "luis@despacho.mx")],
            subject: "Novedades de octubre".into(),
            date: Some("2026-10-25T08:00:00Z".into()),
            body_text: "Conoce nuestras novedades.".into(),
            unread: true,
            ..Default::default()
        },
    ]
}

fn opts(lang: Lang) -> TaskOptions {
    TaskOptions {
        lang,
        me: "Luis Pérez <luis@despacho.mx>".into(),
        today: NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(),
        time_zone: "America/Mexico_City".into(),
        ..Default::default()
    }
}

fn check(name: &str, lang: Lang, payload: &str) {
    assert!(!payload.contains("{{"), "{name}: marcador sin sustituir");
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(format!("{name}.{}.txt", lang.code()));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, payload).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("falta {}; ejecuta con UPDATE_SNAPSHOTS=1", path.display()));
    assert_eq!(payload, expected, "la instantánea {} cambió (UPDATE_SNAPSHOTS=1 si es a propósito)", path.display());
}

#[test]
fn prompt_snapshots() {
    // Local: sin enmascarar. Externo: enmascarado (se comprueba en `classify`).
    let local = Target { label: "Iurefficient · despacho.iurefficient.com".into(), external: false };
    let external = Target { label: "Anthropic · claude-sonnet-5-5".into(), external: true };
    let t = thread();
    for lang in [Lang::Es, Lang::En] {
        let o = opts(lang);
        check(
            "draft_reply",
            lang,
            &tasks::prepare_draft_reply(&local, &t[..1], Tone::Formal, Lang::Es, "acepta y propone el jueves", &o)
                .scope
                .payload,
        );
        check(
            "rewrite",
            lang,
            &tasks::prepare_rewrite(&local, "oye manda el contrato ya", Tone::Formal, Length::Longer, Lang::En, &o)
                .scope
                .payload,
        );
        check("summarize_thread", lang, &tasks::prepare_summarize_thread(&local, &t, &o).scope.payload);
        check("extract", lang, &tasks::prepare_extract(&local, &t[0], &o).scope.payload);
        let classify = tasks::prepare_classify_lead_or_ticket(&external, &t[0], &o);
        assert!(classify.scope.redacted && !classify.scope.payload.contains("ana@cliente.mx"));
        check("classify", lang, &classify.scope.payload);
        check(
            "meeting_minutes",
            lang,
            &tasks::prepare_meeting_minutes(
                &local,
                "[Ana] Revisamos el contrato.\n[Luis] Lo envío el jueves.",
                Some("Revisión de contrato"),
                &o,
            )
            .scope
            .payload,
        );
        let triage = tasks::prepare_triage_today(&local, &t, &o);
        assert!(triage.scope.payload.contains("(id: msg-2)"));
        check("triage_today", lang, &triage.scope.payload);
        check(
            "document_outline",
            lang,
            &tasks::prepare_document_outline(&local, &t[..1], "memorándum para el cliente", &o).scope.payload,
        );
    }
}
