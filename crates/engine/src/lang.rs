//! Idioma de los textos que produce el motor (avisos, etiquetas de comandos).
//!
//! El motor (L4) no depende de egui, así que no usa el `lang` del ui-kit: guarda su propia copia
//! del idioma y la UI llama a los dos `set_lang` cuando cambia.

use std::sync::atomic::{AtomicU8, Ordering};

/// Idioma de la interfaz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    En,
    #[default]
    Es,
}

static LANG: AtomicU8 = AtomicU8::new(1);

/// Fija el idioma del motor.
pub fn set_lang(l: Lang) {
    LANG.store(if l == Lang::En { 0 } else { 1 }, Ordering::Relaxed);
}

/// Idioma actual.
pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 0 { Lang::En } else { Lang::Es }
}

/// `true` si la interfaz está en español.
pub fn spanish() -> bool {
    lang() == Lang::Es
}

/// Elige el texto del idioma actual.
pub fn pick<'a>(en: &'a str, es: &'a str) -> &'a str {
    if spanish() { es } else { en }
}

/// Idioma a partir de una etiqueta (`es`, `es_MX.UTF-8`, `en-US`…). Lo desconocido es inglés.
pub fn from_tag(tag: &str) -> Lang {
    if tag.trim().to_ascii_lowercase().starts_with("es") { Lang::Es } else { Lang::En }
}

/// Un texto en los dos idiomas (avisos que se muestran más tarde, cuando el idioma pudo cambiar).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Text {
    pub en: String,
    pub es: String,
}

impl Text {
    pub fn new(en: impl Into<String>, es: impl Into<String>) -> Self {
        Text { en: en.into(), es: es.into() }
    }
    /// El texto en el idioma actual.
    pub fn get(&self) -> &str {
        pick(&self.en, &self.es)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        assert_eq!(from_tag("es_MX.UTF-8"), Lang::Es);
        assert_eq!(from_tag("en-US"), Lang::En);
        assert_eq!(from_tag(""), Lang::En);
    }
}
