//! `VCARD` (3.0 y 4.0) ⇄ [`Contact`].
//!
//! Igual que con iCalendar, [`patch_contact`] solo reescribe las propiedades de los campos que
//! cambiaron; grupos (`item1.EMAIL`), `X-ABLabel`, `CATEGORIES`, `IMPP`, etc. se conservan. La
//! foto viaja aparte como bytes ([`contact_from_vcard`] la devuelve; quien llama la guarda en el
//! almacén de blobs y pone la huella en `Contact::photo`).

use base64::Engine as _;
use chrono::{DateTime, NaiveDate, Utc};
use iuredesk_dav::ical::Property;
use iuredesk_dav::value::{self, split_unescaped};
use iuredesk_dav::vcard::VCard;
use iuredesk_model::{Contact, Labeled, PostalAddress};

use super::ical::replace_all;

/// Tamaño máximo de una foto aceptada (los servidores suelen limitar a 256 KB–1 MB).
pub const MAX_PHOTO_BYTES: usize = 4 * 1024 * 1024;

fn labeled(values: Vec<iuredesk_dav::vcard::TypedValue>) -> Vec<Labeled> {
    values
        .into_iter()
        .take(100)
        .map(|t| Labeled { label: t.types.into_iter().find(|x| x != "voice").unwrap_or_default(), value: t.value })
        .collect()
}

fn address(p: &Property) -> PostalAddress {
    let parts = split_unescaped(&p.value, ';');
    let at = |i: usize| parts.get(i).cloned().unwrap_or_default();
    let label = p
        .param_values("TYPE")
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .find(|t| !t.is_empty() && t != "pref")
        .unwrap_or_default();
    PostalAddress { label, street: at(2), city: at(3), region: at(4), postal_code: at(5), country: at(6) }
}

fn parse_bday(raw: &str) -> Option<NaiveDate> {
    let date: String = raw.trim().split(['T', 't']).next()?.chars().filter(char::is_ascii_digit).collect();
    if raw.trim().starts_with("--") || date.len() != 8 {
        return None;
    }
    NaiveDate::parse_from_str(&date, "%Y%m%d").ok()
}

fn parse_rev(raw: &str) -> Option<DateTime<Utc>> {
    match value::parse_date_time(raw, None, None).ok()? {
        value::DateTimeValue::Utc(u) => Some(u),
        other => Some(other.naive().and_utc()),
    }
}

/// Bytes de la foto (`PHOTO;ENCODING=b` de 3.0 o `data:` URI de 4.0). Las URL remotas no se
/// descargan (privacidad): `None`.
pub fn photo_bytes(card: &VCard) -> Option<Vec<u8>> {
    let p = card.component.prop("PHOTO")?;
    let v = p.value.trim();
    let b64 = if let Some(rest) = v.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        if !meta.to_ascii_lowercase().ends_with(";base64") {
            return None;
        }
        data
    } else if p.param("ENCODING").is_some_and(|e| e.eq_ignore_ascii_case("b") || e.eq_ignore_ascii_case("base64")) {
        v
    } else {
        return None;
    };
    if b64.len() > MAX_PHOTO_BYTES.saturating_mul(2) {
        return None;
    }
    let clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean.as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(clean.trim_end_matches('=').as_bytes()))
        .ok()?;
    (bytes.len() <= MAX_PHOTO_BYTES && !bytes.is_empty()).then_some(bytes)
}

/// Tipo MIME de una imagen por sus primeros bytes.
pub fn image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() > 12 && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

/// Lee una tarjeta. `id`, `address_book`, `photo`, `remote_id` y `etag` los pone quien llama;
/// la foto se devuelve aparte.
pub fn contact_from_vcard(card: &VCard) -> (Contact, Option<Vec<u8>>) {
    let n = card.name().unwrap_or_default();
    let c = Contact {
        uid: card.uid().unwrap_or_default(),
        full_name: card.display_name().unwrap_or_default(),
        given_name: n.given,
        family_name: n.family,
        nickname: card
            .component
            .text("NICKNAME")
            .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
            .unwrap_or_default(),
        emails: labeled(card.emails()),
        phones: labeled(card.phones()),
        org: card.organization().unwrap_or_default(),
        title: card.component.text("TITLE").unwrap_or_default(),
        addresses: card.component.props("ADR").take(100).map(address).collect(),
        notes: card.note().unwrap_or_default(),
        birthday: card.component.prop("BDAY").and_then(|p| parse_bday(&p.value)),
        updated: card.component.prop("REV").and_then(|p| parse_rev(&p.value)),
        ..Default::default()
    };
    (c, photo_bytes(card))
}

fn is_v4(card: &VCard) -> bool {
    card.version().starts_with('4')
}

fn typed_prop(name: &str, l: &Labeled) -> Property {
    let mut p = Property::text(name, &l.value);
    if !l.label.is_empty() {
        p.set_param("TYPE", &l.label);
    }
    p
}

/// Reescribe `EMAIL`/`TEL`: la línea original se conserva si su valor sigue en el mismo lugar.
fn patch_typed(card: &mut VCard, name: &str, values: &[Labeled], parse: fn(&VCard) -> Vec<Labeled>) {
    let old_props: Vec<Property> = card.component.props(name).cloned().collect();
    let old_vals = parse(card);
    let props = values
        .iter()
        .enumerate()
        .map(|(i, l)| match (old_props.get(i), old_vals.get(i)) {
            (Some(p), Some(o)) if o == l => p.clone(),
            (Some(p), Some(o)) if o.value == l.value => {
                let mut p = p.clone();
                p.remove_param("TYPE");
                if !l.label.is_empty() {
                    p.set_param("TYPE", &l.label);
                }
                p
            }
            _ => typed_prop(name, l),
        })
        .collect();
    replace_all(&mut card.component, name, props);
}

fn adr_prop(a: &PostalAddress) -> Property {
    let esc = value::escape_text;
    let raw =
        format!(";;{};{};{};{};{}", esc(&a.street), esc(&a.city), esc(&a.region), esc(&a.postal_code), esc(&a.country));
    let mut p = Property::new("ADR", &raw);
    if !a.label.is_empty() {
        p.set_param("TYPE", &a.label);
    }
    p
}

/// Aplica a la tarjeta los cambios `old` → `new`. `photo` son los bytes de la foto nueva (solo se
/// usan si `old.photo != new.photo`). Devuelve `true` si cambió algo.
pub fn patch_contact(card: &mut VCard, old: &Contact, new: &Contact, photo: Option<&[u8]>, now: DateTime<Utc>) -> bool {
    let mut changed = false;
    if old.full_name != new.full_name {
        card.set_display_name(&new.full_name);
        changed = true;
    }
    if old.given_name != new.given_name || old.family_name != new.family_name {
        let mut parts = card.component.prop("N").map(|p| split_unescaped(&p.value, ';')).unwrap_or_default();
        parts.resize(parts.len().max(5), String::new());
        if let Some(f) = parts.get_mut(0) {
            f.clone_from(&new.family_name);
        }
        if let Some(g) = parts.get_mut(1) {
            g.clone_from(&new.given_name);
        }
        let raw = parts.iter().map(|s| value::escape_text(s)).collect::<Vec<_>>().join(";");
        match card.component.prop_mut("N") {
            Some(p) => p.value = raw,
            None => card.component.push(Property::new("N", &raw)),
        }
        changed = true;
    }
    if old.nickname != new.nickname {
        set_or_remove(card, "NICKNAME", &new.nickname);
        changed = true;
    }
    if old.emails != new.emails {
        patch_typed(card, "EMAIL", &new.emails, |c| labeled(c.emails()));
        changed = true;
    }
    if old.phones != new.phones {
        patch_typed(card, "TEL", &new.phones, |c| labeled(c.phones()));
        changed = true;
    }
    if old.org != new.org {
        let mut parts = card.component.prop("ORG").map(|p| split_unescaped(&p.value, ';')).unwrap_or_default();
        if parts.is_empty() {
            parts.push(String::new());
        }
        if let Some(first) = parts.get_mut(0) {
            first.clone_from(&new.org);
        }
        if parts.iter().all(String::is_empty) {
            card.component.remove("ORG");
        } else {
            let raw = parts.iter().map(|s| value::escape_text(s)).collect::<Vec<_>>().join(";");
            match card.component.prop_mut("ORG") {
                Some(p) => p.value = raw,
                None => card.component.push(Property::new("ORG", &raw)),
            }
        }
        changed = true;
    }
    if old.title != new.title {
        set_or_remove(card, "TITLE", &new.title);
        changed = true;
    }
    if old.addresses != new.addresses {
        let old_props: Vec<Property> = card.component.props("ADR").cloned().collect();
        let props = new
            .addresses
            .iter()
            .enumerate()
            .map(|(i, a)| match (old_props.get(i), old.addresses.get(i)) {
                (Some(p), Some(o)) if o == a => p.clone(),
                _ => adr_prop(a),
            })
            .collect();
        replace_all(&mut card.component, "ADR", props);
        changed = true;
    }
    if old.notes != new.notes {
        set_or_remove(card, "NOTE", &new.notes);
        changed = true;
    }
    if old.birthday != new.birthday {
        match new.birthday {
            Some(d) => {
                let v = if is_v4(card) { d.format("%Y%m%d") } else { d.format("%Y-%m-%d") };
                replace_all(&mut card.component, "BDAY", vec![Property::new("BDAY", &v.to_string())]);
            }
            None => card.component.remove("BDAY"),
        }
        changed = true;
    }
    if old.photo != new.photo {
        match photo.filter(|_| new.photo.is_some()) {
            Some(bytes) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                let mime = image_mime(bytes);
                let p = if is_v4(card) {
                    Property::new("PHOTO", &format!("data:{mime};base64,{b64}"))
                } else {
                    let kind = mime.trim_start_matches("image/").to_ascii_uppercase();
                    Property::new("PHOTO", &b64).with_param("ENCODING", "b").with_param("TYPE", &kind)
                };
                replace_all(&mut card.component, "PHOTO", vec![p]);
            }
            None => card.component.remove("PHOTO"),
        }
        changed = true;
    }
    if changed {
        card.component.set(Property::new("REV", &value::format_utc(now)));
    }
    changed
}

fn set_or_remove(card: &mut VCard, name: &str, text: &str) {
    if text.is_empty() {
        card.component.remove(name);
    } else {
        match card.component.prop_mut(name) {
            Some(p) => p.value = value::escape_text(text),
            None => card.component.push(Property::text(name, text)),
        }
    }
}

/// Tarjeta nueva (vCard 3.0: la entienden todos los servidores, iCloud incluido).
pub fn new_vcard(c: &Contact, photo: Option<&[u8]>, now: DateTime<Utc>) -> VCard {
    let mut card = VCard::new(&c.uid, &c.full_name);
    if let Some(v) = card.component.prop_mut("VERSION") {
        v.value = "3.0".into();
    }
    card.component.push(Property::new("N", ";;;;"));
    card.component.push(Property::new("PRODID", iuredesk_dav::calendar::PRODID));
    let blank = Contact { uid: c.uid.clone(), full_name: c.full_name.clone(), ..Default::default() };
    patch_contact(&mut card, &blank, c, photo, now);
    card.component.set(Property::new("REV", &value::format_utc(now)));
    card
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    const APPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iPhone OS 18//EN\r\n\
N:Pérez;Ana;;Dra.;\r\nFN:Dra. Ana Pérez\r\nORG:ACME;Ventas\r\nTITLE:Directora\r\n\
item1.EMAIL;type=INTERNET;type=WORK;type=pref:ana@acme.com\r\nitem1.X-ABLabel:_$!<Work>!$_\r\n\
TEL;type=CELL;type=VOICE;type=pref:+52 55 1234 5678\r\n\
ADR;type=HOME:;;Calle 1\\, 2;CDMX;CDMX;01000;México\r\nBDAY:1980-05-17\r\n\
PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ==\r\nX-SOCIALPROFILE;type=twitter:x.com/ana\r\n\
UID:abc-123\r\nEND:VCARD\r\n";

    #[test]
    fn reads_apple_card() {
        let card = VCard::parse(APPLE).unwrap();
        let (c, photo) = contact_from_vcard(&card);
        assert_eq!(c.uid, "abc-123");
        assert_eq!(c.full_name, "Dra. Ana Pérez");
        assert_eq!((c.given_name.as_str(), c.family_name.as_str()), ("Ana", "Pérez"));
        assert_eq!(c.emails, vec![Labeled::new("work", "ana@acme.com")]);
        assert_eq!(c.phones, vec![Labeled::new("cell", "+52 55 1234 5678")]);
        assert_eq!(c.org, "ACME");
        assert_eq!(c.addresses[0].street, "Calle 1, 2");
        assert_eq!(c.addresses[0].label, "home");
        assert_eq!(c.birthday, NaiveDate::from_ymd_opt(1980, 5, 17));
        assert_eq!(photo.unwrap()[..2], [0xff, 0xd8]);
    }

    #[test]
    fn patch_keeps_groups_and_unknown() {
        let mut card = VCard::parse(APPLE).unwrap();
        let (old, _) = contact_from_vcard(&card);
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).unwrap();
        assert!(!patch_contact(&mut card, &old, &old.clone(), None, now));
        assert_eq!(card.to_vcf(), VCard::parse(APPLE).unwrap().to_vcf());
        let mut new = old.clone();
        new.phones.push(Labeled::new("work", "+52 55 0000 0000"));
        new.org = "ACME Corp".into();
        new.family_name = "Pérez Gil".into();
        assert!(patch_contact(&mut card, &old, &new, None, now));
        let out = card.to_vcf();
        assert!(out.contains("item1.EMAIL;TYPE=INTERNET;TYPE=WORK;TYPE=pref:ana@acme.com"));
        assert!(out.contains("item1.X-ABLABEL:_$!<Work>!$_"));
        assert!(out.contains("X-SOCIALPROFILE"));
        assert!(out.contains("ORG:ACME Corp;Ventas"));
        assert!(out.contains("N:Pérez Gil;Ana;;Dra.;"));
        assert!(out.contains("PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ=="));
        let (back, _) = contact_from_vcard(&VCard::parse(&out).unwrap());
        assert_eq!(back.phones, new.phones);
        assert_eq!(back.org, new.org);
    }

    #[test]
    fn photo_v4_and_new_card() {
        let png = b"\x89PNG\r\n\x1a\nxxxx".to_vec();
        let c = Contact {
            uid: "n1".into(),
            full_name: "Juan Ruiz".into(),
            given_name: "Juan".into(),
            family_name: "Ruiz".into(),
            emails: vec![Labeled::new("home", "juan@example.com")],
            birthday: NaiveDate::from_ymd_opt(1990, 1, 2),
            photo: Some(iuredesk_model::BlobHash([1; 32])),
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).unwrap();
        let card = new_vcard(&c, Some(&png), now);
        let (back, photo) = contact_from_vcard(&VCard::parse(&card.to_vcf()).unwrap());
        assert_eq!(photo.as_deref(), Some(png.as_slice()));
        assert_eq!(back.emails, c.emails);
        assert_eq!(back.birthday, c.birthday);
        assert_eq!(back.family_name, "Ruiz");

        let v4 = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nPHOTO:data:image/png;base64,iVBORw0KGgo=\r\nEND:VCARD\r\n";
        assert_eq!(photo_bytes(&VCard::parse(v4).unwrap()).unwrap()[..4], *b"\x89PNG");
        let remote = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nPHOTO:https://x/y.jpg\r\nEND:VCARD\r\n";
        assert!(photo_bytes(&VCard::parse(remote).unwrap()).is_none());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "\\PC{0,300}") {
            if let Ok(card) = VCard::parse(&format!("BEGIN:VCARD\r\nVERSION:3.0\r\n{s}\r\nEND:VCARD\r\n")) {
                let (c, _) = contact_from_vcard(&card);
                let mut card2 = card.clone();
                let mut n = c.clone();
                n.full_name.push('x');
                n.emails.push(Labeled::new("", "a@b.c"));
                patch_contact(&mut card2, &c, &n, None, Utc::now());
            }
        }
    }
}
