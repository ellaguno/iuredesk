//! Vistas tipadas sobre `VCARD` (vCard 3.0, RFC 2426, y 4.0, RFC 6350; 2.1 se lee igual).
//!
//! Como en iCalendar, el componente se conserva entero: lo que no se toca se reescribe tal cual.

use crate::ical::{self, Component, Property};
use crate::value::split_unescaped;
use crate::{Error, Result};

/// Un contacto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCard {
    /// El componente `VCARD`.
    pub component: Component,
}

/// Valor con tipos (`EMAIL;TYPE=work,pref:…`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedValue {
    /// Valor sin escapes.
    pub value: String,
    /// Tipos en minúsculas (`work`, `home`, `cell`, `voice`…); sin `pref`.
    pub types: Vec<String>,
    /// Preferido (`TYPE=pref` en 3.0, `PREF=1` en 4.0).
    pub preferred: bool,
}

/// Nombre estructurado (`N`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Name {
    /// Apellidos.
    pub family: String,
    /// Nombre de pila.
    pub given: String,
    /// Otros nombres.
    pub additional: String,
    /// Prefijo (Dr., Lic.).
    pub prefix: String,
    /// Sufijo.
    pub suffix: String,
}

impl VCard {
    /// Lee todas las tarjetas de un `.vcf`.
    pub fn parse_all(input: &str) -> Result<Vec<Self>> {
        Ok(ical::parse(input)?.into_iter().filter(|c| c.name == "VCARD").map(|component| Self { component }).collect())
    }

    /// Lee la primera tarjeta.
    pub fn parse(input: &str) -> Result<Self> {
        Self::parse_all(input)?.into_iter().next().ok_or(Error::Missing("VCARD"))
    }

    /// Tarjeta nueva (4.0) con `UID` y nombre para mostrar.
    pub fn new(uid: &str, formatted_name: &str) -> Self {
        let mut component = Component::new("VCARD");
        component.push(Property::new("VERSION", "4.0"));
        component.push(Property::text("UID", uid));
        component.push(Property::text("FN", formatted_name));
        Self { component }
    }

    /// Texto `.vcf`.
    pub fn to_vcf(&self) -> String {
        self.component.to_ical_string()
    }

    /// `VERSION` (por omisión `3.0`).
    pub fn version(&self) -> String {
        self.component.text("VERSION").map(|v| v.trim().to_string()).unwrap_or_else(|| "3.0".into())
    }

    /// `UID` (en 4.0 puede venir como `urn:uuid:…`; se devuelve tal cual).
    pub fn uid(&self) -> Option<String> {
        self.component.text("UID").map(|u| u.trim().to_string()).filter(|u| !u.is_empty())
    }

    /// Nombre para mostrar (`FN`); si falta, se arma con `N`, y si no, con el primer correo.
    pub fn display_name(&self) -> Option<String> {
        if let Some(f) = self.component.text("FN").map(|f| f.trim().to_string()).filter(|f| !f.is_empty()) {
            return Some(f);
        }
        if let Some(n) = self.name() {
            let joined = [n.prefix, n.given, n.additional, n.family, n.suffix]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !joined.is_empty() {
                return Some(joined);
            }
        }
        self.emails().into_iter().next().map(|e| e.value)
    }

    /// Nombre estructurado.
    pub fn name(&self) -> Option<Name> {
        let p = self.component.prop("N")?;
        let mut parts = split_unescaped(&p.value, ';').into_iter();
        let mut next = || parts.next().unwrap_or_default();
        Some(Name { family: next(), given: next(), additional: next(), prefix: next(), suffix: next() })
    }

    /// Organización (primer nivel de `ORG`).
    pub fn organization(&self) -> Option<String> {
        let p = self.component.prop("ORG")?;
        split_unescaped(&p.value, ';').into_iter().next().filter(|o| !o.is_empty())
    }

    /// Correos.
    pub fn emails(&self) -> Vec<TypedValue> {
        self.typed("EMAIL")
    }

    /// Teléfonos (en 4.0 pueden venir como `tel:+52…`; se quita el prefijo).
    pub fn phones(&self) -> Vec<TypedValue> {
        self.typed("TEL")
            .into_iter()
            .map(|mut t| {
                if let Some(rest) = t.value.strip_prefix("tel:") {
                    t.value = rest.to_string();
                }
                t
            })
            .collect()
    }

    /// `NOTE`.
    pub fn note(&self) -> Option<String> {
        self.component.text("NOTE")
    }

    /// Cambia el nombre para mostrar.
    pub fn set_display_name(&mut self, name: &str) {
        self.component.set(Property::text("FN", name));
    }

    fn typed(&self, prop: &str) -> Vec<TypedValue> {
        self.component
            .props(prop)
            .map(|p| {
                let mut types = Vec::new();
                let mut preferred = false;
                // TYPE=a,b ; TYPE=a;TYPE=b ; y en 2.1, parámetros sin valor (TEL;WORK;VOICE).
                let raw = p
                    .params
                    .iter()
                    .flat_map(|param| {
                        if param.name == "TYPE" {
                            param.values.iter().flat_map(|v| v.split(',')).map(str::to_string).collect::<Vec<_>>()
                        } else if param.values.is_empty() {
                            vec![param.name.clone()]
                        } else {
                            Vec::new()
                        }
                    })
                    .collect::<Vec<_>>();
                for t in raw {
                    let t = t.trim().to_ascii_lowercase();
                    if t == "pref" {
                        preferred = true;
                    } else if !t.is_empty() && t != "internet" {
                        types.push(t);
                    }
                }
                if p.param("PREF").is_some() {
                    preferred = true;
                }
                TypedValue { value: p.text_value().trim().to_string(), types, preferred }
            })
            .filter(|t| !t.value.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcard3_and_4() {
        let src = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:López;Ana;;Dra.;\r\nFN:Dra. Ana López\r\nORG:ACME\\, S.A.;Ventas\r\n\
EMAIL;TYPE=INTERNET,WORK,pref:ana@acme.mx\r\nitem1.TEL;type=CELL:+52 55 1234\r\nX-ABLabel:raro\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:1\r\nN:Pérez;Juan;;;\r\nEMAIL;TYPE=home;PREF=1:j@p.mx\r\n\
TEL;VALUE=uri;TYPE=voice:tel:+52-1\r\nEND:VCARD\r\n";
        let cards = VCard::parse_all(src).unwrap();
        assert_eq!(cards.len(), 2);
        let a = &cards[0];
        assert_eq!(a.display_name().as_deref(), Some("Dra. Ana López"));
        assert_eq!(a.organization().as_deref(), Some("ACME, S.A."));
        let e = &a.emails()[0];
        assert_eq!(e.types, vec!["work"]);
        assert!(e.preferred);
        assert_eq!(a.phones()[0].types, vec!["cell"]);
        assert_eq!(a.name().unwrap().given, "Ana");
        // Conserva lo desconocido.
        assert!(a.to_vcf().contains("X-ABLABEL:raro"));
        let b = &cards[1];
        assert_eq!(b.version(), "4.0");
        assert_eq!(b.display_name().as_deref(), Some("Juan Pérez"));
        assert!(b.emails()[0].preferred);
        assert_eq!(b.phones()[0].value, "+52-1");
    }

    #[test]
    fn vcard21_types() {
        let c = VCard::parse("BEGIN:VCARD\nVERSION:2.1\nTEL;WORK;VOICE:123\nEMAIL:x@y.z\nEND:VCARD\n").unwrap();
        assert_eq!(c.phones()[0].types, vec!["work", "voice"]);
        assert_eq!(c.display_name().as_deref(), Some("x@y.z"));
    }

    #[test]
    fn new_and_edit() {
        let mut c = VCard::new("u1", "Ana");
        c.set_display_name("Ana, la jefa");
        let back = VCard::parse(&c.to_vcf()).unwrap();
        assert_eq!(back.display_name().as_deref(), Some("Ana, la jefa"));
        assert_eq!(back.uid().as_deref(), Some("u1"));
    }
}
