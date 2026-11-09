//! Buzones compartidos y delegados de Microsoft 365 (F8.1): se abren con el inicio de sesión de
//! una cuenta propia y se tratan como una cuenta más (`Account::shared_of`). Antes de guardarlo se
//! comprueba en Graph que el usuario tenga acceso, para explicar en lenguaje humano lo que falte.

use std::sync::Arc;
use std::time::Duration;

use iuredesk_model::account::{Account, AccountKind, AuthMethod, Identity, OAuthProvider};

use super::{EngineJob, Notice, Session};
use crate::JobOutput;
use crate::background::JobId;
use crate::lang::Text;

/// Tiempo máximo de la comprobación de acceso.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Por qué no se pudo abrir un buzón compartido.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedMailboxFailure {
    pub text: Text,
    /// Hay que volver a iniciar sesión en la cuenta dueña (falta el permiso `.Shared`).
    pub reauth_owner: bool,
}

/// «Agregar buzón compartido» en curso o con su último resultado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedMailboxAdd {
    /// Cuenta con cuyo inicio de sesión se abre.
    pub owner: String,
    pub address: String,
    pub(crate) job: Option<JobId>,
    pub(crate) account: Option<Account>,
    pub failure: Option<SharedMailboxFailure>,
}

impl SharedMailboxAdd {
    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    /// Se agregó (la UI cierra su formulario y llama a [`Session::close_shared_add`]).
    pub fn done(&self) -> bool {
        self.job.is_none() && self.failure.is_none()
    }
}

/// ¿Esta cuenta puede abrir buzones compartidos? (Microsoft 365 con OAuth y que no sea ella misma
/// un buzón compartido).
pub fn can_open_shared(a: &Account) -> bool {
    a.kind == AccountKind::Microsoft365
        && a.shared_of.is_none()
        && matches!(a.incoming.auth, AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft })
}

/// Dirección razonable (lo demás lo dice el servidor).
fn plausible_address(s: &str) -> bool {
    let s = s.trim();
    s.len() <= 320
        && !s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '?' | '#' | '\\'))
        && s.split_once('@').is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.'))
}

/// La cuenta del buzón compartido: la del dueño con otra dirección e id.
fn shared_account(owner: &Account, address: &str, id: String) -> Account {
    let mut a = owner.clone();
    a.id = id;
    a.email = address.to_string();
    a.display_name = address.to_string();
    a.identities =
        vec![Identity { name: address.to_string(), email: address.to_string(), signature: None, reply_to: None }];
    a.caldav_url = None;
    a.carddav_url = None;
    a.shared_of = Some(owner.id.clone());
    a
}

/// Traduce el fallo de la comprobación.
fn failure(e: &iuredesk_graph::GraphError, owner: &str, address: &str) -> SharedMailboxFailure {
    use iuredesk_graph::GraphError as G;
    let plain = |text| SharedMailboxFailure { text, reauth_owner: false };
    match e {
        e if e.needs_reauth() => SharedMailboxFailure {
            text: Text::new(
                format!(
                    "To open shared mailboxes IureDesk needs one more permission from {owner}. Sign in to it again and accept; then add {address} again."
                ),
                format!(
                    "Para abrir buzones compartidos, IureDesk necesita un permiso más de {owner}. Vuelve a iniciar sesión en esa cuenta y acepta; luego agrega {address} otra vez."
                ),
            ),
            reauth_owner: true,
        },
        G::Http { status: 401 | 403, .. } => plain(Text::new(
            format!(
                "{owner} has no access to {address}. Ask your administrator to give you «Full Access» to that mailbox (it can take up to an hour to apply)."
            ),
            format!(
                "{owner} no tiene acceso a {address}. Pide a tu administrador que te dé «Acceso total» a ese buzón (puede tardar hasta una hora en aplicarse)."
            ),
        )),
        G::Http { status: 404, .. } => plain(Text::new(
            format!("There is no mailbox {address} in your organization. Check the address."),
            format!("No hay un buzón {address} en tu organización. Revisa la dirección."),
        )),
        G::Network(_) | G::Throttled { .. } => plain(Text::new(
            "Microsoft 365 could not be reached. Check your connection and try again.",
            "No se pudo hablar con Microsoft 365. Revisa tu conexión y vuelve a intentarlo.",
        )),
        other => plain(Text::new(
            format!("Microsoft 365 did not open {address} ({other})."),
            format!("Microsoft 365 no abrió {address} ({other})."),
        )),
    }
}

impl Session {
    /// Cuentas que pueden abrir buzones compartidos.
    pub fn can_open_shared(&self, account_id: &str) -> bool {
        self.accounts.iter().any(|a| a.id == account_id && can_open_shared(a))
    }

    /// Buzones compartidos abiertos con la cuenta `owner`.
    pub fn shared_mailboxes_of(&self, owner: &str) -> Vec<&Account> {
        self.accounts.iter().filter(|a| a.shared_of.as_deref() == Some(owner)).collect()
    }

    /// F8.1: abre el buzón compartido o delegado `address` con el inicio de sesión de `owner`.
    /// Comprueba el acceso en segundo plano; el resultado queda en [`Session::shared_add`] y, si
    /// funcionó, el buzón se guarda como una cuenta más y empieza a sincronizar.
    pub fn add_shared_mailbox(&mut self, owner: &str, address: &str) -> std::result::Result<(), Text> {
        let tokens: Arc<dyn iuredesk_oauth::TokenProvider> = match &self.live.tokens {
            Some(t) => Arc::clone(t) as Arc<dyn iuredesk_oauth::TokenProvider>,
            None => {
                let http = iuredesk_oauth::http_client().map_err(|e| Text::new(e.message(false), e.message(true)))?;
                Arc::new(iuredesk_oauth::TokenManager::from_settings(
                    http,
                    Arc::clone(&self.secrets),
                    &self.oauth_settings(),
                ))
            }
        };
        let base = self
            .sync_options
            .as_ref()
            .map(|o| o.graph_base_url.clone())
            .unwrap_or_else(|| iuredesk_graph::GRAPH_BASE.to_string());
        self.add_shared_mailbox_with(owner, address, tokens, &base)
    }

    /// [`Session::add_shared_mailbox`] con el proveedor de tokens y la raíz de Graph dados
    /// (pruebas contra un Graph falso).
    #[doc(hidden)]
    pub fn add_shared_mailbox_with(
        &mut self,
        owner: &str,
        address: &str,
        tokens: Arc<dyn iuredesk_oauth::TokenProvider>,
        graph_base: &str,
    ) -> std::result::Result<(), Text> {
        let address = address.trim().to_ascii_lowercase();
        let Some(owner_acc) = self.accounts.iter().find(|a| a.id == owner && can_open_shared(a)).cloned() else {
            return Err(Text::new(
                "Shared mailboxes are opened with a Microsoft 365 account signed in with Microsoft.",
                "Los buzones compartidos se abren con una cuenta de Microsoft 365 con inicio de sesión de Microsoft.",
            ));
        };
        if !plausible_address(&address) {
            return Err(Text::new(
                "Write the mailbox address, like sales@contoso.com.",
                "Escribe la dirección del buzón, como ventas@contoso.com.",
            ));
        }
        if self.accounts.iter().any(|a| a.email.trim().eq_ignore_ascii_case(&address)) {
            return Err(Text::new(
                format!("{address} is already in IureDesk."),
                format!("{address} ya está en IureDesk."),
            ));
        }
        let id = format!("{}-shared-{}", owner_acc.id, self.accounts.len().saturating_add(1));
        let id = (1..1000)
            .map(|n| if n == 1 { id.clone() } else { format!("{id}-{n}") })
            .find(|c| !self.accounts.iter().any(|a| &a.id == c))
            .unwrap_or(id);
        let account = shared_account(&owner_acc, &address, id);
        let client = iuredesk_graph::GraphClient::new(account.clone(), tokens, graph_base, CHECK_TIMEOUT)
            .map_err(|e| Text::new(e.to_string(), e.to_string()))?
            .with_retry(1, Duration::from_secs(5));
        let (owner_email, addr) = (owner_acc.email.clone(), address.clone());
        let job = self.bg.spawn(async move {
            let r = client.get_json(&client.url("/me/mailFolders/inbox?$select=id")).await;
            JobOutput::Engine(EngineJob::SharedMailbox(r.map(|_| ()).map_err(|e| failure(&e, &owner_email, &addr))))
        });
        self.shared_add = Some(SharedMailboxAdd {
            owner: owner_acc.id,
            address,
            job: Some(job),
            account: Some(account),
            failure: None,
        });
        Ok(())
    }

    /// Cierra «Agregar buzón compartido» (si la comprobación sigue, su resultado se descarta).
    pub fn close_shared_add(&mut self) {
        self.shared_add = None;
    }

    pub(crate) fn on_shared_mailbox(
        &mut self,
        id: JobId,
        r: std::result::Result<(), SharedMailboxFailure>,
    ) -> Option<Notice> {
        let add = self.shared_add.as_mut().filter(|s| s.job == Some(id))?;
        add.job = None;
        match r {
            Err(f) => {
                add.failure = Some(f);
                None
            }
            Ok(()) => {
                let account = add.account.take()?;
                let aid = match self.store.new_account_id() {
                    Ok(a) => a,
                    Err(e) => return Some(Notice::error(crate::EngineError::Store(e.to_string()).text())),
                };
                if let Err(e) = self.finish_account(&account, aid) {
                    return Some(Notice::error(e.text()));
                }
                self.restart_sync();
                self.refresh_transports();
                self.rebuild_rows();
                Some(Notice::success(
                    format!("{} was added. Its mail is on its way.", account.email),
                    format!("Se agregó {}. Su correo viene en camino.", account.email),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert!(plausible_address("ventas@contoso.com"));
        assert!(plausible_address(" Ventas+mx@contoso.com.mx "));
        assert!(!plausible_address("ventas"));
        assert!(!plausible_address("ventas@contoso"));
        assert!(!plausible_address("a b@contoso.com"));
        assert!(!plausible_address("x/../me@contoso.com"));
    }

    #[test]
    fn failures_in_human_words() {
        use iuredesk_graph::GraphError as G;
        let f = failure(&G::Http { status: 403, code: "ErrorAccessDenied".into() }, "ana@contoso.com", "v@contoso.com");
        assert!(f.text.es.contains("Acceso total") && !f.reauth_owner);
        let f = failure(&G::Http { status: 404, code: String::new() }, "ana@contoso.com", "v@contoso.com");
        assert!(f.text.es.contains("Revisa la dirección"));
        let f = failure(
            &G::OAuth(iuredesk_oauth::Error::NeedsReconsent { provider: "Microsoft".into() }),
            "ana@contoso.com",
            "v@contoso.com",
        );
        assert!(f.reauth_owner && f.text.es.contains("Vuelve a iniciar sesión"));
    }
}
