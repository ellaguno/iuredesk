//! Contacto de EWS (`t:Contact`) ⇄ JSON de un contacto de Graph (F8.4), para reutilizar
//! [`crate::map::graph::contact_from_graph`] y [`crate::map::graph::contact_patch`].
//!
//! Los campos indexados de EWS (correos `EmailAddress1..3`, teléfonos por clave, direcciones
//! `Home`/`Business`/`Other`) se leen y se escriben uno a uno con `IndexedFieldURI`. Solo se tocan
//! las claves que el modelo conoce (móvil, casa y trabajo): un fax o un teléfono del coche quedan
//! en el servidor tal cual aunque se edite el contacto.

use iuredesk_ews::Element;
use iuredesk_ews::soap::{T, esc};
use serde_json::{Map, Value, json};

fn t<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
    e.child(T, name)
}

fn text(e: &Element, name: &str, max: usize) -> String {
    t(e, name).map(|c| c.text_trim().chars().take(max).collect::<String>()).unwrap_or_default()
}

/// Las entradas `t:Entry Key=…` de un campo indexado: (clave, elemento).
fn entries<'a>(e: &'a Element, name: &str) -> Vec<(&'a str, &'a Element)> {
    t(e, name)
        .map(|l| l.children.iter().take(50).filter_map(|x| x.attr("Key").map(|k| (k, x))).collect())
        .unwrap_or_default()
}

/// Correos (máximo de EWS).
const EMAIL_KEYS: [&str; 3] = ["EmailAddress1", "EmailAddress2", "EmailAddress3"];
/// Teléfonos que el modelo conoce, por grupo de Graph.
const MOBILE: &str = "MobilePhone";
const HOME_PHONES: [&str; 2] = ["HomePhone", "HomePhone2"];
const WORK_PHONES: [&str; 2] = ["BusinessPhone", "BusinessPhone2"];
/// Direcciones: clave de EWS y campo de Graph.
const ADDRESSES: [(&str, &str); 3] =
    [("Home", "homeAddress"), ("Business", "businessAddress"), ("Other", "otherAddress")];
/// Partes de una dirección: elemento de EWS y campo de Graph.
const ADDRESS_PARTS: [(&str, &str); 5] = [
    ("Street", "street"),
    ("City", "city"),
    ("State", "state"),
    ("CountryOrRegion", "countryOrRegion"),
    ("PostalCode", "postalCode"),
];

/// `t:Contact` → JSON de contacto de Graph.
pub fn contact_json(e: &Element) -> Option<Value> {
    let id_el = t(e, "ItemId")?;
    let id = id_el.attr("Id").filter(|i| !i.is_empty() && i.len() <= 2_000)?;
    let emails: Vec<Value> = entries(e, "EmailAddresses")
        .into_iter()
        .filter(|(k, _)| EMAIL_KEYS.contains(k))
        .map(|(_, x)| x.text_trim().trim_start_matches("SMTP:").trim_start_matches("smtp:").to_string())
        .filter(|a| a.contains('@') && a.len() <= 320)
        .map(|a| json!({ "address": a, "name": a }))
        .collect();
    let phones = entries(e, "PhoneNumbers");
    let phone = |keys: &[&str]| -> Vec<String> {
        keys.iter()
            .filter_map(|k| phones.iter().find(|(pk, _)| pk == k))
            .map(|(_, x)| x.text_trim().chars().take(100).collect::<String>())
            .filter(|p| !p.is_empty())
            .collect()
    };
    let mut v = json!({
        "id": id,
        "@odata.etag": id_el.attr("ChangeKey").unwrap_or_default(),
        "displayName": text(e, "DisplayName", 300),
        "givenName": text(e, "GivenName", 200),
        "surname": text(e, "Surname", 200),
        "middleName": text(e, "MiddleName", 200),
        "nickName": text(e, "Nickname", 200),
        "fileAs": text(e, "FileAs", 300),
        "companyName": text(e, "CompanyName", 300),
        "jobTitle": text(e, "JobTitle", 300),
        "department": text(e, "Department", 300),
        "personalNotes": t(e, "Body").map(|b| b.text.chars().take(100_000).collect::<String>()).unwrap_or_default(),
        "emailAddresses": emails,
        "mobilePhone": phone(&[MOBILE]).into_iter().next().unwrap_or_default(),
        "homePhones": phone(&HOME_PHONES),
        "businessPhones": phone(&WORK_PHONES),
    });
    let o = v.as_object_mut()?;
    let addrs = entries(e, "PhysicalAddresses");
    for (key, field) in ADDRESSES {
        if let Some((_, a)) = addrs.iter().find(|(k, _)| *k == key) {
            let mut m = Map::new();
            for (x, g) in ADDRESS_PARTS {
                m.insert(g.into(), json!(text(a, x, 300)));
            }
            o.insert(field.into(), Value::Object(m));
        }
    }
    if let Some(b) = t(e, "Birthday").map(|b| b.text_trim()).filter(|b| b.len() >= 10) {
        o.insert("birthday".into(), json!(b.chars().take(40).collect::<String>()));
    }
    if let Some(m) = t(e, "LastModifiedTime").map(|m| m.text_trim()).filter(|m| !m.is_empty()) {
        o.insert("lastModifiedDateTime".into(), json!(m.chars().take(40).collect::<String>()));
    }
    Some(v)
}

/// Un campo de contacto traducido.
enum Field {
    /// Campo simple: `FieldURI`, XML dentro de `t:Contact` (o `None` para borrarlo), orden.
    Plain { uri: &'static str, xml: Option<String>, order: u8 },
    /// Campo indexado: `FieldURI`, índice, XML dentro de `t:Contact` (o `None`), orden.
    Indexed { uri: &'static str, index: &'static str, xml: Option<String>, order: u8 },
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn simple(patch: &Map<String, Value>, out: &mut Vec<Field>) {
    for (key, uri, tag, order) in [
        ("personalNotes", "item:Body", "", 0u8),
        ("displayName", "contacts:DisplayName", "DisplayName", 1),
        ("givenName", "contacts:GivenName", "GivenName", 2),
        ("nickName", "contacts:Nickname", "Nickname", 3),
        ("companyName", "contacts:CompanyName", "CompanyName", 4),
        ("jobTitle", "contacts:JobTitle", "JobTitle", 10),
        ("surname", "contacts:Surname", "Surname", 11),
    ] {
        let Some(v) = patch.get(key) else { continue };
        let val = v.as_str().unwrap_or_default().trim();
        let xml = (!val.is_empty()).then(|| match tag {
            "" => format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(val)),
            _ => format!("<t:{tag}>{}</t:{tag}>", esc(val)),
        });
        out.push(Field::Plain { uri, xml, order });
    }
    if let Some(b) = patch.get("birthday") {
        let xml = b.as_str().and_then(|d| d.get(..10)).map(|d| format!("<t:Birthday>{d}T00:00:00Z</t:Birthday>"));
        out.push(Field::Plain { uri: "contacts:Birthday", xml, order: 8 });
    }
}

fn indexed(patch: &Map<String, Value>, out: &mut Vec<Field>) {
    if let Some(list) = patch.get("emailAddresses").and_then(Value::as_array) {
        let addrs: Vec<&str> = list.iter().map(|e| s(e, "address").trim()).filter(|a| !a.is_empty()).collect();
        for (i, key) in EMAIL_KEYS.iter().enumerate() {
            let xml = addrs
                .get(i)
                .map(|a| format!(r#"<t:EmailAddresses><t:Entry Key="{key}">{}</t:Entry></t:EmailAddresses>"#, esc(a)));
            out.push(Field::Indexed { uri: "contacts:EmailAddress", index: key, xml, order: 5 });
        }
    }
    if patch.contains_key("mobilePhone") || patch.contains_key("homePhones") || patch.contains_key("businessPhones") {
        let list = |k: &str| -> Vec<String> {
            patch
                .get(k)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        };
        let mobile: Vec<String> = patch
            .get("mobilePhone")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .into_iter()
            .collect();
        for (keys, values) in [
            (&[MOBILE][..], mobile),
            (&HOME_PHONES[..], list("homePhones")),
            (&WORK_PHONES[..], list("businessPhones")),
        ] {
            for (i, key) in keys.iter().enumerate() {
                let xml = values
                    .get(i)
                    .map(|p| format!(r#"<t:PhoneNumbers><t:Entry Key="{key}">{}</t:Entry></t:PhoneNumbers>"#, esc(p)));
                out.push(Field::Indexed { uri: "contacts:PhoneNumber", index: key, xml, order: 7 });
            }
        }
    }
    for (key, field) in ADDRESSES {
        let Some(a) = patch.get(field) else { continue };
        for (part, g) in ADDRESS_PARTS {
            let uri = match part {
                "Street" => "contacts:PhysicalAddress:Street",
                "City" => "contacts:PhysicalAddress:City",
                "State" => "contacts:PhysicalAddress:State",
                "CountryOrRegion" => "contacts:PhysicalAddress:CountryOrRegion",
                _ => "contacts:PhysicalAddress:PostalCode",
            };
            let val = s(a, g).trim();
            let xml = (!val.is_empty()).then(|| {
                format!(r#"<t:PhysicalAddresses><t:Entry Key="{key}"><t:{part}>{}</t:{part}></t:Entry></t:PhysicalAddresses>"#, esc(val))
            });
            out.push(Field::Indexed { uri, index: key, xml, order: 6 });
        }
    }
}

fn fields(patch: &Map<String, Value>) -> Vec<Field> {
    let mut out = Vec::new();
    simple(patch, &mut out);
    indexed(patch, &mut out);
    out
}

/// Contenido de `t:Contact` para crear un contacto (en el orden del esquema; las entradas de un
/// mismo campo indexado van juntas).
pub fn create_xml(patch: &Map<String, Value>) -> String {
    let mut plain: Vec<(u8, String)> = Vec::new();
    // Entradas por campo indexado: (orden, etiqueta) → entradas.
    let mut groups: Vec<(u8, &'static str, Vec<String>)> = Vec::new();
    for f in fields(patch) {
        match f {
            Field::Plain { xml: Some(x), order, .. } => plain.push((order, x)),
            Field::Indexed { xml: Some(x), order, uri, .. } => {
                let tag = match uri {
                    "contacts:EmailAddress" => "EmailAddresses",
                    "contacts:PhoneNumber" => "PhoneNumbers",
                    _ => "PhysicalAddresses",
                };
                // Quitar la envoltura: se junta abajo.
                let inner = x
                    .strip_prefix(&format!("<t:{tag}>"))
                    .and_then(|r| r.strip_suffix(&format!("</t:{tag}>")))
                    .unwrap_or(&x)
                    .to_string();
                match groups.iter_mut().find(|(_, t, _)| *t == tag) {
                    Some((_, _, v)) => v.push(inner),
                    None => groups.push((order, tag, vec![inner])),
                }
            }
            _ => {}
        }
    }
    for (order, tag, entries) in groups {
        let body = if tag == "PhysicalAddresses" { merge_addresses(&entries) } else { entries.concat() };
        plain.push((order, format!("<t:{tag}>{body}</t:{tag}>")));
    }
    plain.sort_by_key(|(o, _)| *o);
    plain.into_iter().map(|(_, x)| x).collect()
}

/// Junta las partes de cada dirección en una sola entrada por clave (`Home`, `Business`…), con
/// las partes en el orden del esquema.
fn merge_addresses(entries: &[String]) -> String {
    let mut out = String::new();
    for (key, _) in ADDRESSES {
        let open = format!(r#"<t:Entry Key="{key}">"#);
        let parts: Vec<&str> =
            entries.iter().filter_map(|e| e.strip_prefix(&open).and_then(|r| r.strip_suffix("</t:Entry>"))).collect();
        if !parts.is_empty() {
            out.push_str(&open);
            for (part, _) in ADDRESS_PARTS {
                if let Some(p) = parts.iter().find(|p| p.starts_with(&format!("<t:{part}>"))) {
                    out.push_str(p);
                }
            }
            out.push_str("</t:Entry>");
        }
    }
    out
}

/// `t:SetItemField`/`t:DeleteItemField` para cambiar un contacto (vacío: nada que cambiar).
pub fn update_xml(patch: &Map<String, Value>) -> String {
    fields(patch)
        .into_iter()
        .map(|f| match f {
            Field::Plain { uri, xml: Some(x), .. } => {
                format!(r#"<t:SetItemField><t:FieldURI FieldURI="{uri}"/><t:Contact>{x}</t:Contact></t:SetItemField>"#)
            }
            Field::Plain { uri, xml: None, .. } => {
                format!(r#"<t:DeleteItemField><t:FieldURI FieldURI="{uri}"/></t:DeleteItemField>"#)
            }
            Field::Indexed { uri, index, xml: Some(x), .. } => format!(
                r#"<t:SetItemField><t:IndexedFieldURI FieldURI="{uri}" FieldIndex="{index}"/><t:Contact>{x}</t:Contact></t:SetItemField>"#
            ),
            Field::Indexed { uri, index, xml: None, .. } => format!(
                r#"<t:DeleteItemField><t:IndexedFieldURI FieldURI="{uri}" FieldIndex="{index}"/></t:DeleteItemField>"#
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::graph::{contact_from_graph, contact_patch};

    const C: &str = r#"<t:Contact xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <t:ItemId Id="C1" ChangeKey="ck1"/>
      <t:Body BodyType="Text">Cliente desde 2020</t:Body>
      <t:FileAs>Gómez, Luis</t:FileAs>
      <t:DisplayName>Luis Gómez</t:DisplayName>
      <t:GivenName>Luis</t:GivenName>
      <t:CompanyName>Contoso</t:CompanyName>
      <t:EmailAddresses><t:Entry Key="EmailAddress1">luis@contoso.com</t:Entry><t:Entry Key="EmailAddress2">SMTP:luis@gmail.com</t:Entry></t:EmailAddresses>
      <t:PhysicalAddresses><t:Entry Key="Business"><t:Street>Av. Juárez 10</t:Street><t:City>Monterrey</t:City><t:State>NL</t:State><t:CountryOrRegion>México</t:CountryOrRegion><t:PostalCode>64000</t:PostalCode></t:Entry></t:PhysicalAddresses>
      <t:PhoneNumbers><t:Entry Key="BusinessPhone">+52 81 1234</t:Entry><t:Entry Key="MobilePhone">+52 81 9999</t:Entry><t:Entry Key="BusinessFax">+52 81 0000</t:Entry></t:PhoneNumbers>
      <t:Birthday>1985-04-12T00:00:00Z</t:Birthday>
      <t:JobTitle>Ventas</t:JobTitle>
      <t:Surname>Gómez</t:Surname>
    </t:Contact>"#;

    fn contact() -> iuredesk_model::Contact {
        let e = iuredesk_dav::dav::xml::parse(C.as_bytes()).unwrap();
        contact_from_graph(&contact_json(&e).unwrap())
    }

    #[test]
    fn ews_contact_becomes_a_contact() {
        let c = contact();
        assert_eq!(c.remote_id.as_deref(), Some("C1"));
        assert_eq!(c.etag.as_deref(), Some("ck1"));
        assert_eq!(
            (c.full_name.as_str(), c.given_name.as_str(), c.family_name.as_str()),
            ("Luis Gómez", "Luis", "Gómez")
        );
        assert_eq!(
            c.emails.iter().map(|e| e.value.as_str()).collect::<Vec<_>>(),
            ["luis@contoso.com", "luis@gmail.com"]
        );
        let phones: Vec<(&str, &str)> = c.phones.iter().map(|p| (p.label.as_str(), p.value.as_str())).collect();
        assert_eq!(phones, [("cell", "+52 81 9999"), ("work", "+52 81 1234")], "el fax no se lee");
        assert_eq!((c.org.as_str(), c.title.as_str(), c.notes.as_str()), ("Contoso", "Ventas", "Cliente desde 2020"));
        let a = c.addresses.first().unwrap();
        assert_eq!((a.label.as_str(), a.city.as_str(), a.postal_code.as_str()), ("work", "Monterrey", "64000"));
        assert_eq!(c.birthday.map(|d| d.to_string()).as_deref(), Some("1985-04-12"));
    }

    #[test]
    fn edits_touch_only_what_changed() {
        let old = contact();
        let mut new = old.clone();
        new.emails.remove(1);
        new.title = "Director de ventas".into();
        let u = update_xml(&contact_patch(Some(&old), &new));
        assert!(
            u.contains(
                r#"<t:FieldURI FieldURI="contacts:JobTitle"/><t:Contact><t:JobTitle>Director de ventas</t:JobTitle>"#
            ),
            "{u}"
        );
        assert!(u.contains(
            r#"<t:SetItemField><t:IndexedFieldURI FieldURI="contacts:EmailAddress" FieldIndex="EmailAddress1"/>"#
        ));
        assert!(u.contains(r#"<t:DeleteItemField><t:IndexedFieldURI FieldURI="contacts:EmailAddress" FieldIndex="EmailAddress2"/></t:DeleteItemField>"#), "{u}");
        assert!(!u.contains("PhoneNumber") && !u.contains("PhysicalAddress") && !u.contains("DisplayName"), "{u}");
        assert!(update_xml(&contact_patch(Some(&old), &old)).is_empty());
    }

    #[test]
    fn create_in_schema_order() {
        let x = create_xml(&contact_patch(None, &contact()));
        let pos = |s: &str| x.find(s).unwrap_or_else(|| panic!("{s} en {x}"));
        assert!(pos("<t:Body") < pos("<t:DisplayName>") && pos("<t:DisplayName>") < pos("<t:GivenName>"));
        assert!(
            pos("<t:CompanyName>") < pos("<t:EmailAddresses>")
                && pos("<t:EmailAddresses>") < pos("<t:PhysicalAddresses>")
        );
        assert!(
            pos("<t:PhysicalAddresses>") < pos("<t:PhoneNumbers>") && pos("<t:PhoneNumbers>") < pos("<t:Birthday>")
        );
        assert!(pos("<t:Birthday>") < pos("<t:JobTitle>") && pos("<t:JobTitle>") < pos("<t:Surname>"));
        assert_eq!(x.matches("<t:EmailAddresses>").count(), 1, "una sola lista de correos: {x}");
        assert!(x.contains(r#"<t:Entry Key="Business"><t:Street>Av. Juárez 10</t:Street><t:City>Monterrey</t:City><t:State>NL</t:State><t:CountryOrRegion>México</t:CountryOrRegion><t:PostalCode>64000</t:PostalCode></t:Entry>"#), "{x}");
        assert!(x.contains(r#"<t:Entry Key="MobilePhone">+52 81 9999</t:Entry>"#));
    }

    #[test]
    fn hostile_contacts_never_panic() {
        for x in [
            r#"<t:Contact xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"/>"#,
            r#"<t:Contact xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><t:ItemId Id="x"/><t:EmailAddresses><t:Entry>sin clave</t:Entry></t:EmailAddresses><t:Birthday>ayer</t:Birthday></t:Contact>"#,
        ] {
            let e = iuredesk_dav::dav::xml::parse(x.as_bytes()).unwrap();
            if let Some(v) = contact_json(&e) {
                let c = contact_from_graph(&v);
                let _ = create_xml(&contact_patch(None, &c));
            }
        }
    }
}
