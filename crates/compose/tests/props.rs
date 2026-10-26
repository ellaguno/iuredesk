//! Propiedades: ninguna entrada hostil hace caer las importaciones ni las conversiones, y el
//! HTML que se genera nunca lleva guiones.

use iuredesk_compose::address::parse_address_list;
use iuredesk_compose::reply::{forward_subject, reply_subject};
use iuredesk_compose::{Lang, RichBody, SubjectStyle};
use proptest::prelude::*;

/// Fragmentos de HTML con los que armar basura «plausible».
fn html_piece() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("<p>".to_string()),
        Just("</p>".to_string()),
        Just("<div class=\"gmail_signature\">".to_string()),
        Just("</div>".to_string()),
        Just("<blockquote>".to_string()),
        Just("</blockquote>".to_string()),
        Just("<ul><li>".to_string()),
        Just("</li></ul>".to_string()),
        Just("<ol start=\"-5\">".to_string()),
        Just("<pre>".to_string()),
        Just("</pre>".to_string()),
        Just("<b><i><u><s>".to_string()),
        Just("</s></u></i></b>".to_string()),
        Just("<a href=\"javascript:alert(1)\">".to_string()),
        Just("<a href=\"https://x.mx\">".to_string()),
        Just("</a>".to_string()),
        Just("<img src=\"cid:x\" width=\"999999999999\">".to_string()),
        Just("<script>alert(1)</script>".to_string()),
        Just("<style>".to_string()),
        Just("<h7>".to_string()),
        Just("<br/>".to_string()),
        Just("<table><tr><td>".to_string()),
        Just("&amp;&#0;&#x10FFFF;&bogus;".to_string()),
        Just("<!--".to_string()),
        "[^<>]{0,20}",
        any::<String>(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn from_html_never_panics(s in any::<String>()) {
        let body = RichBody::from_html(&s);
        let _ = body.to_html();
        let _ = body.to_plain();
        let _ = body.to_markdown();
    }

    #[test]
    fn from_html_structured_garbage(pieces in prop::collection::vec(html_piece(), 0..60)) {
        let html = pieces.concat();
        let body = RichBody::from_html(&html);
        let out = body.to_html().to_ascii_lowercase();
        prop_assert!(!out.contains("<script"));
        prop_assert!(!out.contains("javascript:"));
        let _ = body.to_plain();
        let md = body.to_markdown();
        let _ = RichBody::from_markdown(&md);
    }

    #[test]
    fn from_markdown_never_panics(s in any::<String>()) {
        let body = RichBody::from_markdown(&s);
        let _ = body.to_html();
        let _ = body.to_plain();
    }

    #[test]
    fn address_lists_never_panic(s in any::<String>()) {
        let _ = parse_address_list(&s);
    }

    #[test]
    fn subjects_never_panic(s in any::<String>()) {
        let r = reply_subject(&s, SubjectStyle::Standard);
        prop_assert!(r.starts_with("Re:"));
        let _ = forward_subject(&s, SubjectStyle::Outlook, Lang::Es);
    }

    #[test]
    fn plain_lines_are_short_or_unbreakable(words in prop::collection::vec("[a-zñ]{1,12}", 1..200)) {
        let text = words.join(" ");
        let out = RichBody::from_plain(&text).to_plain();
        for line in out.lines() {
            prop_assert!(line.chars().count() <= 78, "{line:?}");
        }
        let unflowed = out.replace(" \n", " ");
        prop_assert_eq!(unflowed.trim_end(), text.as_str());
    }
}
