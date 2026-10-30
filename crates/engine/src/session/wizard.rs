//! Asistente de alta sin callejones sin salida (UX-2): cuenta que ya existe, «Configurar
//! manualmente» con prueba de cada servidor por separado y fallos del navegador explicados.

use std::sync::Arc;

use iuredesk_accounts::SecretPurpose;

use super::{EngineJob, Notice, Session};
use crate::JobOutput;
use crate::background::JobId;
use crate::lang::Text;
use crate::onboarding::{
    OAuthFailure, OAuthIssue, ProbeStatus, WizardStep, account_from_manual, existing_account, manual_setup,
    needs_manual,
};

/// Tiempo máximo de cada prueba de servidor.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl Session {
    /// Si la dirección escrita ya es de una cuenta, pasa al paso «Esta cuenta ya está en
    /// IureDesk» y devuelve `true` (no al volver a iniciar sesión en ella).
    pub fn wizard_check_existing(&mut self) -> bool {
        let Some(w) = self.wizard.as_ref().filter(|w| w.reauth.is_none()) else { return false };
        let Some(id) = existing_account(&self.accounts, &w.email).map(|a| a.id.clone()) else { return false };
        if let Some(w) = self.wizard.as_mut() {
            w.existing = Some(id);
            w.step = WizardStep::Exists;
            w.message = None;
        }
        true
    }

    /// Abre «Configurar manualmente» con lo que haya encontrado el descubrimiento (o
    /// `mail.<dominio>`). Conserva el formulario si ya se estaba llenando.
    pub fn wizard_manual(&mut self) {
        self.wizard_forget_other_email();
        let Some(w) = self.wizard.as_mut() else { return };
        if let Some(c) = w.cancel.take() {
            c.cancel();
        }
        if w.manual.is_none() {
            w.manual = Some(manual_setup(&w.email, w.discovery.as_ref()));
        }
        w.step = WizardStep::Manual;
        w.message = None;
        w.oauth_issue = None;
        w.job = None;
    }

    /// «Cancelar» mientras se prueba o se espera al navegador: vuelve al paso anterior (tarjeta
    /// del proveedor o formulario manual) sin perder lo escrito.
    pub fn wizard_cancel_working(&mut self) {
        let Some(w) = self.wizard.as_mut() else { return };
        if let Some(c) = w.cancel.take() {
            c.cancel();
        }
        w.job = None;
        w.message = None;
        if let Some(m) = w.manual.as_mut() {
            m.incoming_test = None;
            m.outgoing_test = None;
        }
        w.step = match w.step {
            WizardStep::Working => {
                if w.manual.is_some() && w.flow.is_none() {
                    WizardStep::Manual
                } else {
                    WizardStep::Provider
                }
            }
            s => s,
        };
    }

    /// «Atrás» desde el formulario manual: a la tarjeta del proveedor si la hubo; si no, al correo.
    pub fn wizard_manual_back(&mut self) {
        let to_provider = self
            .wizard
            .as_ref()
            .is_some_and(|w| w.flow.is_some() && w.discovery.as_ref().is_some_and(|d| !needs_manual(d)));
        if to_provider {
            if let Some(w) = self.wizard.as_mut() {
                w.step = WizardStep::Provider;
                w.message = None;
                w.job = None;
                if let Some(m) = w.manual.as_mut() {
                    m.incoming_test = None;
                    m.outgoing_test = None;
                }
            }
        } else {
            self.wizard_back();
        }
    }

    /// «Probar conexión» del formulario manual: prueba entrante y saliente a la vez (cada una con
    /// su resultado) y, si las dos funcionan, guarda las contraseñas y la cuenta.
    pub fn wizard_manual_test(&mut self) {
        let Some(w) = self.wizard.as_ref() else { return };
        let Some(m) = w.manual.clone() else { return };
        if let Some(t) = m.problem() {
            if let Some(w) = self.wizard.as_mut() {
                w.message = Some(Notice::error(t));
            }
            return;
        }
        if w.password.is_empty() {
            if let Some(w) = self.wizard.as_mut() {
                w.message = Some(Notice::error(Text::new("Write your password.", "Escribe tu contraseña.")));
            }
            return;
        }
        let aid = match w.reserved.map(Ok).unwrap_or_else(|| self.store.new_account_id()) {
            Ok(a) => a,
            Err(e) => {
                if let Some(w) = self.wizard.as_mut() {
                    w.message = Some(Notice::error(Text::new(
                        format!("Could not prepare the account: {e}"),
                        format!("No se pudo preparar la cuenta: {e}"),
                    )));
                }
                return;
            }
        };
        let account = account_from_manual(super::new_account_key(aid), &w.name, &w.email, &m);
        let password = w.password.clone();
        let out_password =
            if m.same_password || m.outgoing_password.is_empty() { None } else { Some(m.outgoing_password.clone()) };
        let secrets = Arc::clone(&self.secrets);
        let acc = account.clone();
        let job = self.bg.spawn(async move {
            let timeout = |what: &'static str| {
                Text::new(
                    format!("The {what} server did not answer within a minute. Check the name and port."),
                    format!(
                        "El servidor {} no respondió en un minuto. Revisa el nombre y el puerto.",
                        if what == "incoming" { "de entrada" } else { "de salida" }
                    ),
                )
            };
            let out_pw = out_password.clone().unwrap_or_else(|| password.clone());
            let (incoming, outgoing) = tokio::join!(
                async {
                    tokio::time::timeout(PROBE_TIMEOUT, iuredesk_accounts::test_incoming(&acc.incoming, &password))
                        .await
                        .map_err(|_| timeout("incoming"))?
                        .map(|_| ())
                        .map_err(|e| crate::onboarding::probe_text(&e))
                },
                async {
                    tokio::time::timeout(PROBE_TIMEOUT, iuredesk_accounts::test_outgoing(&acc.outgoing, &out_pw))
                        .await
                        .map_err(|_| timeout("outgoing"))?
                        .map(|_| ())
                        .map_err(|e| crate::onboarding::probe_text(&e))
                },
            );
            let saved = if incoming.is_ok() && outgoing.is_ok() {
                let id = acc.id.clone();
                tokio::task::spawn_blocking(move || {
                    secrets.set(&id, SecretPurpose::Incoming, &password)?;
                    if let Some(p) = out_password {
                        secrets.set(&id, SecretPurpose::Outgoing, &p)?;
                    }
                    Ok::<(), iuredesk_accounts::Error>(())
                })
                .await
                .map_err(|e| Text::new(e.to_string(), e.to_string()))
                .and_then(|r| {
                    r.map_err(|e| {
                        Text::new(
                            format!("The connection works, but the password could not be saved in the system keyring: {e}"),
                            format!("La conexión funciona, pero no se pudo guardar la contraseña en el llavero del sistema: {e}"),
                        )
                    })
                })
            } else {
                Ok(())
            };
            JobOutput::Engine(EngineJob::ManualProbe { incoming, outgoing, saved })
        });
        if let Some(w) = self.wizard.as_mut() {
            w.reserved = Some(aid);
            w.saved = Some(account);
            w.message = None;
            w.job = Some(job);
            if let Some(m) = w.manual.as_mut() {
                m.incoming_test = Some(ProbeStatus::Running);
                m.outgoing_test = Some(ProbeStatus::Running);
            }
        }
    }

    pub(crate) fn wizard_on_manual(
        &mut self,
        id: JobId,
        incoming: std::result::Result<(), Text>,
        outgoing: std::result::Result<(), Text>,
        saved: std::result::Result<(), Text>,
    ) -> Option<Notice> {
        let w = self.wizard.as_mut().filter(|w| w.job == Some(id))?;
        let both = incoming.is_ok() && outgoing.is_ok();
        // El aviso de arriba dice qué falló (el formulario puede no verse entero sin desplazarse).
        let mut why_en = Vec::new();
        let mut why_es = Vec::new();
        if let Err(t) = &incoming {
            why_en.push(format!("Incoming: {}", t.en));
            why_es.push(format!("Entrante: {}", t.es));
        }
        if let Err(t) = &outgoing {
            why_en.push(format!("Outgoing: {}", t.en));
            why_es.push(format!("Saliente: {}", t.es));
        }
        if let Some(m) = w.manual.as_mut() {
            let status = |r: std::result::Result<(), Text>| match r {
                Ok(()) => ProbeStatus::Ok,
                Err(t) => ProbeStatus::Failed(t),
            };
            m.incoming_test = Some(status(incoming));
            m.outgoing_test = Some(status(outgoing));
        }
        if !both {
            w.job = None;
            w.message = Some(Notice::error(Text::new(
                format!("{} Fix it and test again.", why_en.join(" · ")),
                format!("{} Corrígelo y vuelve a probar.", why_es.join(" · ")),
            )));
            return None;
        }
        // Se guarda como cualquier alta (lista de cuentas, carpetas, sincronización).
        let r = self.wizard_on_probe(id, saved, false);
        if let Some(w) = self.wizard.as_mut()
            && w.step == WizardStep::Provider
        {
            // `wizard_on_probe` vuelve a la tarjeta del proveedor si algo falla: aquí, al formulario.
            w.step = WizardStep::Manual;
        }
        r
    }

    pub(crate) fn wizard_on_oauth(&mut self, id: JobId, r: std::result::Result<(), OAuthFailure>) -> Option<Notice> {
        match r {
            Ok(()) => self.wizard_on_probe(id, Ok(()), true),
            Err(f) => {
                let w = self.wizard.as_mut().filter(|w| w.job == Some(id))?;
                w.job = None;
                w.cancel = None;
                w.step = WizardStep::Provider;
                w.message = Some(if f.issue == OAuthIssue::Cancelled {
                    Notice::info(f.text.en, f.text.es)
                } else {
                    Notice::error(f.text)
                });
                w.oauth_issue = Some(f.issue);
                None
            }
        }
    }
}
