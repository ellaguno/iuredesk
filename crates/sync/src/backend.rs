//! Rasgo común de los protocolos de correo: el motor elige uno por cuenta (IMAP, IMAP con
//! etiquetas de Gmail, POP3 o Microsoft Graph) y solo habla con él a través de [`MailBackend`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use iuredesk_model::MessageId;
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol};

use crate::SyncOptions;
use crate::engine::Ctx;
use crate::error::Result;
use crate::imap_sync::CycleReport;

/// Futuro en caja que se puede mandar entre hilos.
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Lo que el motor necesita de un protocolo de correo.
pub(crate) trait MailBackend: Send + Sync {
    /// Contexto de la cuenta.
    fn ctx(&self) -> &Arc<Ctx>;
    /// Una pasada completa (o solo la bandeja de entrada).
    fn cycle(&self, inbox_only: bool) -> BoxFuture<'_, Result<CycleReport>>;
    /// Una tanda de historial antiguo; `true` si queda más.
    fn backfill(&self) -> BoxFuture<'_, Result<bool>>;
    /// Replica la cola de operaciones locales.
    fn replay(&self) -> BoxFuture<'_, Result<()>>;
    /// Baja el cuerpo de un mensaje (al abrirlo).
    fn fetch_body(&self, msg: MessageId) -> BoxFuture<'_, Result<()>>;
}

/// Protocolo elegido para una cuenta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// IMAP (con `X-GM-EXT-1` se activan las etiquetas de Gmail).
    Imap,
    /// POP3.
    Pop3,
    /// Microsoft Graph (Microsoft 365 / Outlook.com con OAuth).
    Graph,
}

/// Elige el protocolo: Microsoft 365 con OAuth va por Graph (si está activado y hay proveedor de
/// tokens); Gmail y el resto, por IMAP; POP3 si la cuenta lo pide.
pub fn choose_backend(account: &Account, opts: &SyncOptions, has_tokens: bool) -> BackendKind {
    if account.incoming.protocol == Protocol::Pop3 {
        return BackendKind::Pop3;
    }
    let oauth = matches!(account.incoming.auth, AuthMethod::OAuth2 { .. });
    if account.kind == AccountKind::Microsoft365 && oauth && has_tokens && opts.graph_mail {
        return BackendKind::Graph;
    }
    BackendKind::Imap
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::account::{OAuthProvider, Security, ServerConfig};

    fn acc(kind: AccountKind, auth: AuthMethod, protocol: Protocol) -> Account {
        let srv = |p| ServerConfig {
            protocol: p,
            host: "h".into(),
            port: 993,
            security: Security::Tls,
            auth,
            username: "u".into(),
        };
        Account {
            id: "a".into(),
            display_name: "A".into(),
            email: "a@b.c".into(),
            kind,
            incoming: srv(protocol),
            outgoing: srv(Protocol::Smtp),
            caldav_url: None,
            carddav_url: None,
            identities: vec![],
        }
    }

    #[test]
    fn microsoft_with_oauth_uses_graph() {
        let o = SyncOptions::default();
        let ms = AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft };
        assert_eq!(choose_backend(&acc(AccountKind::Microsoft365, ms, Protocol::Imap), &o, true), BackendKind::Graph);
        // Sin tokens o con contraseña: IMAP como antes.
        assert_eq!(choose_backend(&acc(AccountKind::Microsoft365, ms, Protocol::Imap), &o, false), BackendKind::Imap);
        assert_eq!(
            choose_backend(&acc(AccountKind::Microsoft365, AuthMethod::Password, Protocol::Imap), &o, true),
            BackendKind::Imap
        );
        let off = SyncOptions { graph_mail: false, ..Default::default() };
        assert_eq!(choose_backend(&acc(AccountKind::Microsoft365, ms, Protocol::Imap), &off, true), BackendKind::Imap);
        let g = AuthMethod::OAuth2 { provider: OAuthProvider::Google };
        assert_eq!(choose_backend(&acc(AccountKind::Gmail, g, Protocol::Imap), &o, true), BackendKind::Imap);
        assert_eq!(
            choose_backend(&acc(AccountKind::Generic, AuthMethod::Password, Protocol::Pop3), &o, true),
            BackendKind::Pop3
        );
    }
}
