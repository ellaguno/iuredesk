//! Editar una cuenta desde «Ajustes → Cuentas»: nombre, identidades, servidores (IMAP/POP/SMTP,
//! CalDAV/CardDAV) con «Probar conexión», sincronizar ya y lo que se borra al quitarla.

use iuredesk_accounts::SecretPurpose;
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider};

use super::{Notice, Outcome, Session};
use crate::background::JobId;
use crate::lang::Text;
use crate::{EngineError, JobOutput, Result};

/// Prueba de conexión en curso o terminada.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionTest {
    /// Probando (con el trabajo de fondo).
    Running(JobId),
    /// Funcionó.
    Ok,
    /// Falló, con el motivo en lenguaje humano.
    Failed(Text),
}

/// Nombre del tipo de cuenta para mostrar.
pub fn kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Gmail => "Gmail",
        AccountKind::Microsoft365 => "Microsoft 365",
        AccountKind::ICloud => "iCloud",
        AccountKind::Iurefficient => "Iurefficient",
        AccountKind::Generic => "IMAP",
    }
}

/// ¿Se pueden editar los servidores a mano? (Gmail, Microsoft 365 e iCloud los fija el proveedor.)
pub fn servers_editable(a: &Account) -> bool {
    matches!(a.kind, AccountKind::Generic) && !matches!(a.incoming.auth, AuthMethod::OAuth2 { .. })
}

/// Lo que falta o está mal en una cuenta editada (`None` si está bien).
pub fn validate_account(a: &Account) -> Option<Text> {
    let email = a.email.trim();
    if email.is_empty() || !email.contains('@') || email.len() > 320 {
        return Some(Text::new("Write a valid email address.", "Escribe una dirección de correo válida."));
    }
    if servers_editable(a) {
        for (s, en, es) in [(&a.incoming, "incoming", "de entrada"), (&a.outgoing, "outgoing", "de salida")] {
            let host = s.host.trim();
            if host.is_empty() || host.len() > 253 || host.contains(char::is_whitespace) {
                return Some(Text::new(
                    format!("The {en} server name is missing or not valid."),
                    format!("Falta el nombre del servidor {es} o no es válido."),
                ));
            }
            if s.port == 0 {
                return Some(Text::new(
                    format!("The {en} server port must be a number from 1 to 65535."),
                    format!("El puerto del servidor {es} debe ser un número de 1 a 65535."),
                ));
            }
        }
    }
    for url in [&a.caldav_url, &a.carddav_url].into_iter().flatten() {
        let u = url.trim();
        if !u.is_empty() && !(u.starts_with("https://") || u.starts_with("http://")) {
            return Some(Text::new(
                "Calendar and contacts addresses must start with https://.",
                "Las direcciones de calendario y contactos deben empezar con https://.",
            ));
        }
    }
    None
}

impl Session {
    /// La cuenta con ese id (para el editor).
    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Guarda una cuenta editada (y la contraseña nueva, si se da, en el llavero). Si cambiaron
    /// los servidores, la sincronización vuelve a arrancar con ellos.
    pub fn save_account(&mut self, edited: &Account, password: Option<&str>) -> Result<Outcome> {
        let Some(old) = self.account(&edited.id).cloned() else {
            return Err(EngineError::Invalid("cuenta inexistente".into()));
        };
        if let Some(t) = validate_account(edited) {
            return Ok(Outcome::Notice(Notice::error(t)));
        }
        let mut a = edited.clone();
        a.email = a.email.trim().to_string();
        a.display_name = a.display_name.trim().to_string();
        a.incoming.host = a.incoming.host.trim().to_string();
        a.outgoing.host = a.outgoing.host.trim().to_string();
        for u in [&mut a.caldav_url, &mut a.carddav_url] {
            if u.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *u = None;
            }
        }
        a.identities.retain(|i| !i.email.trim().is_empty());
        if let Some(st) = &self.account_store {
            st.upsert(a.clone()).map_err(|e| EngineError::Io(e.to_string()))?;
        }
        if let Some(pw) = password.filter(|p| !p.is_empty()) {
            self.secrets
                .set(&a.id, SecretPurpose::Incoming, pw)
                .map_err(|e| EngineError::Io(format!("llavero: {e}")))?;
        }
        if let Some(aid) = self.account_ids.get(&a.id).copied() {
            let _ = self.store.put_account_label(&iuredesk_store::AccountLabel {
                id: aid,
                name: a.display_name.clone(),
                email: a.email.clone(),
            });
        }
        let servers_changed = old.incoming != a.incoming
            || old.outgoing != a.outgoing
            || old.caldav_url != a.caldav_url
            || old.carddav_url != a.carddav_url
            || password.is_some_and(|p| !p.is_empty());
        self.reload_accounts()?;
        if servers_changed {
            self.restart_sync();
        }
        self.refresh_transports();
        self.rebuild_rows();
        Ok(Outcome::Notice(Notice::success(
            format!("Saved the changes to {}", a.email),
            format!("Se guardaron los cambios de {}", a.email),
        )))
    }

    /// Prueba los servidores de una cuenta (editada o no) en segundo plano; el resultado se lee
    /// con [`Session::connection_test`]. Sin contraseña nueva se usa la del llavero.
    pub fn test_account_connection(&mut self, account: &Account, password: Option<&str>) {
        if let AuthMethod::OAuth2 { provider } = account.incoming.auth {
            let who = match provider {
                OAuthProvider::Google => "Google",
                OAuthProvider::Microsoft => "Microsoft",
            };
            self.connection_test = Some((
                account.id.clone(),
                ConnectionTest::Failed(Text::new(
                    format!("This account signs in with {who}; to check it, use «Sign in again»."),
                    format!("Esta cuenta inicia sesión con {who}; para comprobarla usa «Volver a iniciar sesión»."),
                )),
            ));
            return;
        }
        if let Some(t) = validate_account(account) {
            self.connection_test = Some((account.id.clone(), ConnectionTest::Failed(t)));
            return;
        }
        let acc = account.clone();
        let given = password.filter(|p| !p.is_empty()).map(str::to_string);
        let secrets = std::sync::Arc::clone(&self.secrets);
        let job = self.bg.spawn(async move {
            let r = async {
                let password = match given {
                    Some(p) => p,
                    None => {
                        let id = acc.id.clone();
                        tokio::task::spawn_blocking(move || secrets.get(&id, SecretPurpose::Incoming))
                            .await
                            .map_err(|e| Text::new(e.to_string(), e.to_string()))?
                            .ok()
                            .flatten()
                            .ok_or_else(|| {
                                Text::new(
                                    "There is no saved password for this account: write it and try again.",
                                    "No hay contraseña guardada para esta cuenta: escríbela y vuelve a probar.",
                                )
                            })?
                    }
                };
                let test = async {
                    iuredesk_accounts::test_incoming(&acc.incoming, &password)
                        .await
                        .map_err(|e| crate::onboarding::probe_text(&e))?;
                    iuredesk_accounts::test_outgoing(&acc.outgoing, &password)
                        .await
                        .map(|_| ())
                        .map_err(|e| crate::onboarding::probe_text(&e))
                };
                tokio::time::timeout(std::time::Duration::from_secs(60), test).await.map_err(|_| {
                    Text::new("The server did not answer within a minute.", "El servidor no respondió en un minuto.")
                })?
            }
            .await;
            JobOutput::Engine(super::EngineJob::AccountTest(r))
        });
        self.connection_test = Some((account.id.clone(), ConnectionTest::Running(job)));
    }

    /// Estado de la última prueba de conexión de esa cuenta.
    pub fn connection_test(&self, account_id: &str) -> Option<&ConnectionTest> {
        self.connection_test.as_ref().filter(|(id, _)| id == account_id).map(|(_, t)| t)
    }

    /// Olvida la prueba de conexión (al cerrar el editor).
    pub fn clear_connection_test(&mut self) {
        self.connection_test = None;
    }

    pub(crate) fn on_account_test(&mut self, job: JobId, r: std::result::Result<(), Text>) {
        if let Some((_, t)) = self.connection_test.as_mut()
            && *t == ConnectionTest::Running(job)
        {
            *t = match r {
                Ok(()) => ConnectionTest::Ok,
                Err(e) => ConnectionTest::Failed(e),
            };
        }
    }

    /// Fija el estado de sincronización de una cuenta (capturas y pruebas, sin red).
    #[doc(hidden)]
    pub fn demo_account_status(&mut self, id: &str, status: super::AccountSync) {
        if let Some(aid) = self.account_ids.get(id).copied() {
            self.live.status.insert(aid, status);
        }
    }

    /// «Sincronizar ahora» de una cuenta: correo, calendario y contactos.
    pub fn sync_account_now(&mut self, id: &str) {
        if let Some(aid) = self.account_ids.get(id).copied() {
            if let Some(s) = self.live.status.get_mut(&aid)
                && matches!(s, super::AccountSync::Offline(_) | super::AccountSync::Error { .. })
            {
                *s = super::AccountSync::Starting;
            }
            if let Some(h) = &self.live.handle {
                let _ = h.sync_now(id);
            }
        }
        if let Some(p) = &self.pim.handle {
            let _ = p.sync_now(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use iuredesk_model::account::{Protocol, Security, ServerConfig};

    use super::*;

    fn imap() -> Account {
        let server = |protocol, host: &str, port| ServerConfig {
            protocol,
            host: host.into(),
            port,
            security: Security::Tls,
            auth: AuthMethod::Password,
            username: "ana@x.com".into(),
        };
        Account {
            id: "a1".into(),
            display_name: "Ana".into(),
            email: "ana@x.com".into(),
            kind: AccountKind::Generic,
            incoming: server(Protocol::Imap, "imap.x.com", 993),
            outgoing: server(Protocol::Smtp, "smtp.x.com", 465),
            caldav_url: None,
            carddav_url: None,
            identities: Vec::new(),
        }
    }

    #[test]
    fn validation_in_human_words() {
        assert_eq!(validate_account(&imap()), None);
        let mut a = imap();
        a.incoming.host = " ".into();
        assert!(validate_account(&a).is_some_and(|t| t.es.contains("servidor de entrada")));
        let mut a = imap();
        a.outgoing.port = 0;
        assert!(validate_account(&a).is_some_and(|t| t.es.contains("puerto")));
        let mut a = imap();
        a.email = "sin-arroba".into();
        assert!(validate_account(&a).is_some());
        let mut a = imap();
        a.caldav_url = Some("ftp://x".into());
        assert!(validate_account(&a).is_some());
        assert!(servers_editable(&imap()));
        let mut g = imap();
        g.kind = AccountKind::Gmail;
        assert!(!servers_editable(&g));
        assert_eq!(kind_label(AccountKind::Microsoft365), "Microsoft 365");
    }
}
