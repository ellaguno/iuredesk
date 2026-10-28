//! Correo seguro de IureDesk (capa L3): OpenPGP y S/MIME en Rust puro.
//!
//! - [`openpgp`]: claves y operaciones OpenPGP con rPGP (`pgp`, MIT/Apache-2.0). Se eligió rPGP y
//!   no `sequoia-openpgp` porque sequoia es LGPL-2.0-or-later (licencia no aprobada en
//!   `cargo xtask licenses`) y su backend en Rust puro está marcado como experimental.
//! - [`autocrypt`] y [`wkd`]: aprender claves de las cabeceras y del dominio del correo.
//! - [`smime`]: CMS con RustCrypto (`cms`, `x509-cert`, `rsa`, `p256`, `p384`), `.p12` con
//!   `p12-keystore`.
//! - [`secure`]: [`Crypto::seal`] al enviar (PGP/MIME RFC 3156, S/MIME RFC 8551) y
//!   [`Crypto::open`] al leer, con el [`SecurityReport`] de la insignia.
//! - [`keystore`]: claves propias (protegidas en disco; contraseñas en el llavero) y de contactos.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

pub mod autocrypt;
pub mod error;
pub mod keystore;
pub mod mime;
pub mod openpgp;
pub mod report;
pub mod secure;
pub mod smime;
pub mod wkd;

pub use error::{CryptoError, Result};
pub use keystore::{KeyStore, MemoryVault, OwnKeyInfo, Vault};
pub use report::{SecurityReport, SignatureInfo, SignatureState, Technology, Tone};
pub use secure::{Capabilities, Crypto, Opened, SealRequest};
