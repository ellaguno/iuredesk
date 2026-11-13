//! R4.2: la tabla de atajos de la guía de uso sale de los comandos. Si se cambia un comando o un
//! atajo, esta prueba falla hasta regenerarla con
//! `IUREDESK_UPDATE_GUIDE=1 cargo test -p iuredesk-engine --test guide`.

use std::path::PathBuf;

use iuredesk_engine::command::shortcuts_markdown;

fn check(spanish: bool, rel: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/guia").join(rel);
    let want = shortcuts_markdown(spanish);
    if std::env::var_os("IUREDESK_UPDATE_GUIDE").is_some() {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} no está al día: IUREDESK_UPDATE_GUIDE=1 cargo test -p iuredesk-engine --test guide",
        path.display()
    );
}

#[test]
fn shortcut_tables_are_current() {
    check(true, "es/atajos.md");
    check(false, "en/shortcuts.md");
}

#[test]
fn tables_mention_every_scheme_and_the_palette() {
    for spanish in [true, false] {
        let t = shortcuts_markdown(spanish);
        assert!(t.contains("| Outlook | Gmail | Apple Mail |"));
        assert!(t.contains("`Ctrl+K`"));
        assert!(t.contains("`F1`"));
    }
}
