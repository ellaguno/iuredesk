//! Propiedades: ninguna respuesta (bytes cualquiera o XML con forma de EWS rota) hace caer a
//! quien la lee, y el texto escapado vuelve igual.

use iuredesk_dav::dav::xml::parse;
use iuredesk_ews::ops::item;
use iuredesk_ews::soap::{body_response, esc, fault, messages};
use proptest::prelude::*;

const T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        if let Ok(root) = parse(&bytes) {
            let _ = fault(&root);
            let _ = item(&root);
            if let Ok(r) = body_response(root) {
                let _ = messages(&r);
            }
        }
    }

    #[test]
    fn message_fields_never_panic(
        // Ids reales de EWS: base64 (un atributo XML normaliza los espacios en blanco).
        id in "[A-Za-z0-9+/=_-]{1,60}",
        subject in ".{0,200}",
        date in ".{0,40}",
        size in ".{0,20}",
        flag in ".{0,20}",
    ) {
        let xml = format!(
            r#"<t:Message xmlns:t="{T}"><t:ItemId Id="{}"/><t:Subject>{}</t:Subject><t:DateTimeReceived>{}</t:DateTimeReceived><t:Size>{}</t:Size><t:Flag><t:FlagStatus>{}</t:FlagStatus></t:Flag></t:Message>"#,
            esc(&id), esc(&subject), esc(&date), esc(&size), esc(&flag)
        );
        let root = parse(xml.as_bytes()).unwrap();
        if let Some(i) = item(&root) {
            prop_assert_eq!(i.id, id);
            prop_assert!(i.subject.chars().count() <= 1_000);
        }
    }

    #[test]
    fn escaped_text_round_trips(s in "[^\u{0}-\u{8}\u{b}\u{c}\u{e}-\u{1f}\u{fffe}\u{ffff}]{0,200}") {
        let xml = format!(r#"<t:Subject xmlns:t="{T}">{}</t:Subject>"#, esc(&s));
        let root = parse(xml.as_bytes()).unwrap();
        prop_assert_eq!(root.text, s);
    }
}
