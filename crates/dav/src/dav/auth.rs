//! Autenticación HTTP: Basic (contraseña o contraseña de app) y Bearer (OAuth 2).
//!
//! El cliente pide el encabezado `Authorization` antes de cada petición, así un proveedor de
//! tokens OAuth puede renovarlo cuando caduque. Ante un 401 el cliente llama a
//! [`AuthProvider::invalidate`] y, si devuelve `true`, reintenta una vez.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;

use crate::Result;

/// Fuente del encabezado `Authorization`.
pub trait AuthProvider: Send + Sync {
    /// Valor completo del encabezado (`Basic …`, `Bearer …`). Vacío: sin autenticación.
    fn auth_header(&self) -> impl Future<Output = Result<String>> + Send;

    /// El servidor rechazó la credencial: descartar el token en caché. `true` si vale la pena
    /// reintentar (hay un token nuevo que pedir).
    fn invalidate(&self) -> bool {
        false
    }
}

/// Sin autenticación (servidores locales de prueba).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAuth;

impl AuthProvider for NoAuth {
    async fn auth_header(&self) -> Result<String> {
        Ok(String::new())
    }
}

/// Usuario y contraseña (HTTP Basic). iCloud exige una contraseña de app.
#[derive(Clone)]
pub struct BasicAuth {
    header: String,
}

impl BasicAuth {
    /// Credencial Basic.
    pub fn new(user: &str, password: &str) -> Self {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        Self { header: format!("Basic {token}") }
    }
}

impl std::fmt::Debug for BasicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BasicAuth(***)")
    }
}

impl AuthProvider for BasicAuth {
    async fn auth_header(&self) -> Result<String> {
        Ok(self.header.clone())
    }
}

/// Token Bearer fijo. Para OAuth con renovación, implementar [`AuthProvider`] directamente.
#[derive(Clone)]
pub struct BearerAuth {
    token: String,
}

impl BearerAuth {
    /// Token de acceso.
    pub fn new(token: &str) -> Self {
        Self { token: token.to_string() }
    }
}

impl std::fmt::Debug for BearerAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BearerAuth(***)")
    }
}

impl AuthProvider for BearerAuth {
    async fn auth_header(&self) -> Result<String> {
        Ok(format!("Bearer {}", self.token))
    }
}

/// Versión con despacho dinámico, para guardar cualquier proveedor en el cliente.
pub(crate) trait DynAuth: Send + Sync {
    fn header(&self) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>>;
    fn invalidate(&self) -> bool;
}

impl<T: AuthProvider> DynAuth for T {
    fn header(&self) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        Box::pin(self.auth_header())
    }

    fn invalidate(&self) -> bool {
        AuthProvider::invalidate(self)
    }
}

pub(crate) type SharedAuth = Arc<dyn DynAuth>;
