//! Propiedades: los parsers nunca fallan con pánico ante datos arbitrarios de la red.

use iuredesk_autoconfig::email::EmailAddress;
use iuredesk_autoconfig::srv;
use iuredesk_autoconfig::xml::parse_config;
use iuredesk_autoconfig::{base_domain, net::SrvRecord};
use proptest::prelude::*;

const FIXTURE: &str = include_str!("fixtures/autoconfig-example.xml");

proptest! {
    #[test]
    fn email_parse_never_panics(s in "\\PC{0,300}") {
        let _ = EmailAddress::parse(&s);
    }

    #[test]
    fn xml_parse_never_panics(s in "\\PC{0,2000}") {
        let email = EmailAddress::parse("a@b.com").unwrap();
        let _ = parse_config(&s, &email);
    }

    #[test]
    fn xml_parse_survives_mutations(cut in 0usize..2000, insert in "[<>&/\"'a-z%;#x0-9 ]{0,20}") {
        let email = EmailAddress::parse("ana@example.com").unwrap();
        let at = FIXTURE.char_indices().map(|(i, _)| i).nth(cut).unwrap_or(FIXTURE.len());
        let mut xml = FIXTURE.to_string();
        xml.insert_str(at, &insert);
        if let Ok(cfg) = parse_config(&xml, &email) {
            for s in cfg.incoming.iter().chain(cfg.outgoing.iter()) {
                prop_assert!(s.port != 0);
                prop_assert!(!s.host.is_empty());
                prop_assert!(s.host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'));
            }
        }
    }

    #[test]
    fn srv_and_base_domain_never_panic(target in "\\PC{0,80}", p in any::<u16>(), w in any::<u16>(), port in any::<u16>()) {
        let _ = srv::pick(&[SrvRecord { priority: p, weight: w, port, target: target.clone() }]);
        let _ = srv::txt_path(std::slice::from_ref(&target));
        let _ = base_domain(&target);
    }
}
