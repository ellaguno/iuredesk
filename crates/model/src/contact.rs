//! Contactos (modelo cercano a vCard, RFC 6350).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, AddressBookId, BlobHash, ContactId};

/// Libreta de direcciones (colección CardDAV, contactos de Graph/Google, o local).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressBook {
    /// Id local.
    pub id: AddressBookId,
    /// Cuenta.
    pub account: AccountId,
    /// Nombre visible.
    pub name: String,
    /// Ruta o id remoto, opaco.
    pub remote_path: String,
    /// Solo lectura.
    pub read_only: bool,
    /// Estado de sincronización opaco.
    pub sync_token: Option<String>,
}

/// Valor con etiqueta (`work`, `home`, `mobile`…), como `EMAIL;TYPE=work`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labeled {
    /// Etiqueta libre (vacía si no hay).
    pub label: String,
    /// Valor.
    pub value: String,
}

impl Labeled {
    /// Crea un valor etiquetado.
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into() }
    }
}

/// Dirección postal (`ADR`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostalAddress {
    /// Etiqueta (`work`, `home`).
    pub label: String,
    /// Calle y número.
    pub street: String,
    /// Ciudad.
    pub city: String,
    /// Región / estado / provincia.
    pub region: String,
    /// Código postal.
    pub postal_code: String,
    /// País.
    pub country: String,
}

/// Contacto (`VCARD`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// Id local.
    pub id: ContactId,
    /// Libreta.
    pub address_book: AddressBookId,
    /// `UID` de vCard.
    pub uid: String,
    /// Nombre completo para mostrar (`FN`).
    pub full_name: String,
    /// Nombre de pila.
    pub given_name: String,
    /// Apellidos.
    pub family_name: String,
    /// Apodo.
    pub nickname: String,
    /// Correos.
    pub emails: Vec<Labeled>,
    /// Teléfonos.
    pub phones: Vec<Labeled>,
    /// Organización.
    pub org: String,
    /// Cargo.
    pub title: String,
    /// Direcciones postales.
    pub addresses: Vec<PostalAddress>,
    /// Notas.
    pub notes: String,
    /// Cumpleaños.
    pub birthday: Option<NaiveDate>,
    /// Foto en el almacén de blobs.
    pub photo: Option<BlobHash>,
    /// Id o `href` remoto, opaco.
    pub remote_id: Option<String>,
    /// `ETag` remoto, opaco.
    pub etag: Option<String>,
    /// Última modificación.
    pub updated: Option<DateTime<Utc>>,
}

impl Contact {
    /// Correo principal, si hay.
    pub fn primary_email(&self) -> Option<&str> {
        self.emails.first().map(|e| e.value.as_str())
    }
}
