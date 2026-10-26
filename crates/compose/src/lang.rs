//! Idioma de los textos que genera el redactor (línea de cita, encabezado de reenvío, errores).

use serde::{Deserialize, Serialize};

/// Idioma de la interfaz o del correo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    /// Español.
    #[default]
    Es,
    /// Inglés.
    En,
}

impl Lang {
    /// Elige el texto del idioma.
    pub fn pick<'a>(self, en: &'a str, es: &'a str) -> &'a str {
        match self {
            Lang::Es => es,
            Lang::En => en,
        }
    }

    /// Idioma a partir de una etiqueta BCP 47 (`es-MX`, `en_US.UTF-8`…); inglés si no es español.
    pub fn from_tag(tag: &str) -> Self {
        if tag.trim().to_ascii_lowercase().starts_with("es") { Lang::Es } else { Lang::En }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        assert_eq!(Lang::from_tag("es-MX"), Lang::Es);
        assert_eq!(Lang::from_tag("en_US.UTF-8"), Lang::En);
        assert_eq!(Lang::from_tag(""), Lang::En);
        assert_eq!(Lang::Es.pick("a", "b"), "b");
    }
}
