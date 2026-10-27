//! Datos de entrada de las tareas (independientes del almacén: la UI los arma desde `model`).

use serde::{Deserialize, Serialize};

/// Persona de una cabecera.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Addr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
}

impl Addr {
    pub fn new(name: Option<&str>, email: &str) -> Self {
        Self { name: name.map(str::to_string).filter(|n| !n.trim().is_empty()), email: email.to_string() }
    }

    /// «Nombre <correo>» o sólo el correo.
    pub fn display(&self) -> String {
        match &self.name {
            Some(n) => format!("{n} <{}>", self.email),
            None => self.email.clone(),
        }
    }
}

/// Un correo tal como se le puede mostrar al asistente.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EmailInput {
    /// Identificador local (para el triage: la IA lo devuelve tal cual).
    pub id: String,
    pub from: Addr,
    #[serde(default)]
    pub to: Vec<Addr>,
    #[serde(default)]
    pub cc: Vec<Addr>,
    pub subject: String,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// Texto plano del cuerpo (sin citas largas si la UI puede quitarlas).
    #[serde(default)]
    pub body_text: String,
    /// Sólo nombres de los adjuntos; su contenido nunca se envía.
    #[serde(default)]
    pub attachments: Vec<String>,
    /// Si es no leído (triage).
    #[serde(default)]
    pub unread: bool,
}

/// Idioma de las instrucciones y de la respuesta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Es,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Es => "es",
            Lang::En => "en",
        }
    }

    pub fn pick<'a>(self, en: &'a str, es: &'a str) -> &'a str {
        match self {
            Lang::Es => es,
            Lang::En => en,
        }
    }
}
