//! Redactar con el modelo de `iuredesk-compose`: identidades y firmas, nuevo/responder/reenviar
//! desde el crudo del original, borradores guardados en «Borradores», sugerencias de
//! destinatarios y la gestión de cuentas (quitar, volver a iniciar sesión).

use std::sync::Arc;

use chrono::{Local, Offset, Utc};
use iuredesk_compose::signature::IdentitySignatures;
use iuredesk_compose::{
    BuildOptions, Draft, ForwardMode, ReplyMode, ReplyOptions, RichBody, Signature, SignaturePlacement,
};
use iuredesk_model::account::Identity;
use iuredesk_model::{AccountId, Flags, FolderRole, Mailbox, MessageId};
use iuredesk_store::NewMessage;

use super::{Notice, Outcome, Session, store_err};
use crate::compose::{ComposeDraft, ComposeKind, Mailto};
use crate::lang::Text;
use crate::{EngineError, Result};

fn compose_lang() -> iuredesk_compose::Lang {
    if crate::lang::spanish() { iuredesk_compose::Lang::Es } else { iuredesk_compose::Lang::En }
}

/// Una cuenta para los ajustes («Cuentas»).
#[derive(Clone, Debug)]
pub struct AccountRow {
    /// `Account::id` (vacío en el buzón de ejemplo).
    pub id: String,
    pub aid: Option<AccountId>,
    pub name: String,
    pub email: String,
    /// Servidor entrante («imap.gmail.com»).
    pub server: String,
    pub status: Option<super::live::AccountSync>,
    /// Firma (texto) para mensajes nuevos y respuestas.
    pub signature: String,
}

impl Session {
    // ------------------------------------------------------------------ identidades y firmas

    /// Identidades con las que se puede enviar, con su cuenta.
    pub fn identities(&self) -> Vec<(AccountId, Identity)> {
        let mut out: Vec<(AccountId, Identity)> = Vec::new();
        for a in &self.accounts {
            let Some(aid) = self.account_ids.get(&a.id).copied() else { continue };
            if a.identities.is_empty() {
                out.push((aid, a.primary_identity()));
            } else {
                out.extend(a.identities.iter().cloned().map(|i| (aid, i)));
            }
        }
        for l in &self.labels {
            if !out.iter().any(|(a, _)| *a == l.id) {
                out.push((
                    l.id,
                    Identity { name: l.name.clone(), email: l.email.clone(), signature: None, reply_to: None },
                ));
            }
        }
        out
    }

    fn identity_of(&self, account: Option<AccountId>) -> Option<(AccountId, Identity)> {
        let ids = self.identities();
        account.and_then(|a| ids.iter().find(|(x, _)| *x == a).cloned()).or_else(|| ids.into_iter().next())
    }

    /// Cuenta de una identidad (por su dirección).
    pub fn account_of_identity(&self, email: &str) -> Option<AccountId> {
        self.identities().into_iter().find(|(_, i)| i.email.eq_ignore_ascii_case(email)).map(|(a, _)| a)
    }

    /// Firma de una identidad (`reply` = respuesta o reenvío).
    pub fn signature_for(&self, identity: &Identity, reply: bool) -> Option<RichBody> {
        self.signatures.for_identity(identity, reply)
    }

    /// Dónde va la firma al responder.
    pub fn signature_placement(&self) -> SignaturePlacement {
        self.signatures.placement
    }

    /// Texto de la firma de una dirección (para editarla en los ajustes).
    pub fn signature_text(&self, email: &str) -> String {
        let ident = Identity { name: String::new(), email: email.to_string(), signature: None, reply_to: None };
        self.signatures.for_identity(&ident, false).map(|b| b.to_plain().trim_end().to_string()).unwrap_or_default()
    }

    /// Guarda la firma (texto) de una dirección, para mensajes nuevos y respuestas.
    pub fn set_signature_text(&mut self, email: &str, text: &str) {
        let name = format!("firma {email}");
        if text.trim().is_empty() {
            let _ = self.signatures.remove(&name);
            self.signatures.assign(email, IdentitySignatures::default());
        } else {
            self.signatures.upsert(Signature { name: name.clone(), body: RichBody::from_plain(text) });
            self.signatures.assign(email, IdentitySignatures { new_message: Some(name.clone()), reply: Some(name) });
        }
        if let Err(e) = self.signatures.save(&self.store) {
            self.notices.push(Notice::error(Text::new(
                format!("The signature could not be saved: {e}"),
                format!("No se pudo guardar la firma: {e}"),
            )));
        }
    }

    // ------------------------------------------------------------------ redactar

    /// Mensaje nuevo desde una cuenta (o la primera), con su firma.
    pub fn compose_new(&self, account: Option<AccountId>) -> ComposeDraft {
        let Some((aid, ident)) = self.identity_of(account) else {
            return ComposeDraft::new(
                None,
                Identity { name: String::new(), email: String::new(), signature: None, reply_to: None },
            );
        };
        let mut cd = ComposeDraft::new(Some(aid), ident.clone());
        let sig = self.signature_for(&ident, false);
        iuredesk_compose::signature::apply(&mut cd.draft.body, sig.as_ref(), self.signature_placement());
        cd
    }

    /// Mensaje nuevo desde un enlace `mailto:`.
    pub fn compose_mailto(&self, url: &str) -> Option<ComposeDraft> {
        let m = Mailto::parse(url)?;
        let mut cd = self.compose_new(None);
        let mut d = crate::compose::draft_from_mailto(&m, cd.draft.identity.clone());
        let sig = self.signature_for(&d.identity, false);
        iuredesk_compose::signature::apply(&mut d.body, sig.as_ref(), self.signature_placement());
        cd.draft = d;
        Some(cd)
    }

    /// Crudo del original para responder: el descargado o, si aún no está, uno mínimo armado con el
    /// sobre y el extracto.
    fn raw_for_reply(&mut self, id: MessageId) -> Option<Vec<u8>> {
        if let Ok(raw) = self.store.get_raw(id) {
            return Some(raw);
        }
        let env = self.envelope(id)?.clone();
        let text = self.bodies.get(&id).map(|b| b.text.clone()).unwrap_or_else(|| env.preview.clone());
        let mut raw = iuredesk_store::sample::render_rfc822(&env, &text);
        if raw.is_empty() {
            raw = text.into_bytes();
        }
        Some(raw)
    }

    /// Responder, responder a todos o reenviar el mensaje enfocado con los constructores de
    /// `iuredesk-compose` (identidad que recibió el original, `Re:`/`Fwd:`, cita y atribución en el
    /// idioma de la interfaz, firma de respuesta).
    pub(crate) fn reply_like(&mut self, base: &str) -> Result<Outcome> {
        let id = self.focused.ok_or(EngineError::NoSelection)?;
        let env = self.envelope(id).cloned().ok_or(EngineError::NoSelection)?;
        let raw = self.raw_for_reply(id).ok_or(EngineError::NoSelection)?;
        let identities: Vec<Identity> = {
            // La de la cuenta del original va primero (es la que se elige si no hay coincidencia).
            let mut all = self.identities();
            all.sort_by_key(|(a, _)| *a != env.account);
            all.into_iter().map(|(_, i)| i).collect()
        };
        let sig_ident = identities.first().cloned();
        let opts = ReplyOptions {
            lang: compose_lang(),
            subject_style: Default::default(),
            tz: Some(Local::now().offset().fix()),
            signature: sig_ident.as_ref().and_then(|i| self.signature_for(i, true)),
            placement: self.signature_placement(),
            original_id: Some(id),
        };
        let (kind, built) = match base {
            "mail.forward" => {
                (ComposeKind::Forward, iuredesk_compose::forward(&raw, &identities, ForwardMode::Inline, &opts))
            }
            "mail.reply_all" => {
                (ComposeKind::ReplyAll, iuredesk_compose::reply(&raw, &identities, ReplyMode::All, &opts))
            }
            _ => (ComposeKind::Reply, iuredesk_compose::reply(&raw, &identities, ReplyMode::Sender, &opts)),
        };
        let draft = built.map_err(|e| EngineError::Invalid(e.message(compose_lang())))?;
        let account = self.account_of_identity(&draft.identity.email).or(Some(env.account));
        Ok(Outcome::Compose(Box::new(ComposeDraft { kind, account, draft, saved: None })))
    }

    /// Cambia la identidad «De:» del borrador y su firma.
    pub fn switch_identity(&self, cd: &mut ComposeDraft, account: AccountId, identity: Identity) {
        let reply = cd.kind != ComposeKind::New;
        let sig = self.signature_for(&identity, reply);
        cd.draft.switch_identity(identity, sig.as_ref(), self.signature_placement());
        cd.account = Some(account);
    }

    /// Direcciones sugeridas para lo que se está escribiendo (contactos).
    pub fn suggest_addresses(&self, text: &str, limit: usize) -> Vec<Mailbox> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Mailbox> = Vec::new();
        for c in self.store.find_contacts(&needle, limit.saturating_mul(2).min(200)).unwrap_or_default() {
            let name_hit = c.full_name.to_lowercase().contains(&needle);
            for e in &c.emails {
                if name_hit || e.value.to_lowercase().contains(&needle) {
                    let m = if c.full_name.trim().is_empty() {
                        Mailbox::addr(e.value.clone())
                    } else {
                        Mailbox::new(c.full_name.clone(), e.value.clone())
                    };
                    if !out.iter().any(|x| x.addr.eq_ignore_ascii_case(&m.addr)) {
                        out.push(m);
                    }
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        out.truncate(limit);
        out
    }

    // ------------------------------------------------------------------ borradores

    /// Crudo de un borrador: MIME completo si ya se puede construir; si no (sin destinatarios),
    /// texto plano.
    fn draft_raw(&self, d: &Draft, with_attachments: bool) -> Vec<u8> {
        let mut d = d.clone();
        if !with_attachments {
            d.attachments.clear();
        }
        let mid = iuredesk_compose::generate_message_id(&d.identity.email);
        let opts = BuildOptions { message_id: Some(mid.clone()), ..BuildOptions::now() };
        match iuredesk_compose::build_message(&d, &iuredesk_compose::FsResolver, &opts) {
            Ok(b) => b.raw,
            Err(_) => crate::compose::plain_rfc822(&d, Utc::now(), &mid),
        }
    }

    /// Guarda el borrador en «Borradores» de su cuenta, en lugar de la versión anterior.
    /// `upload` = también en el servidor (al cerrar o con «Guardar borrador»); el guardado
    /// automático mientras se escribe es solo local.
    pub fn save_draft(&mut self, cd: &mut ComposeDraft, upload: bool) -> Result<MessageId> {
        let account = cd.account.or_else(|| self.labels.first().map(|l| l.id)).ok_or(EngineError::NoAccount)?;
        let folder = self.role_folder(account, FolderRole::Drafts).ok_or_else(|| {
            EngineError::Invalid(
                crate::lang::pick(
                    "this account has no Drafts folder yet",
                    "esta cuenta aún no tiene carpeta de borradores",
                )
                .to_string(),
            )
        })?;
        let raw = self.draft_raw(&cd.draft, upload);
        if let Some(prev) = cd.saved.take() {
            self.discard_message(prev);
        }
        let flags = Flags { seen: true, draft: true, ..Default::default() };
        let id = if upload && self.is_live() {
            let id = iuredesk_sync::ops::append_message(&self.store, folder, raw, flags)
                .map_err(|e| EngineError::Store(e.to_string()))?;
            self.kick(account);
            id
        } else {
            let (mut env, text) = iuredesk_sync::parse::envelope_from_raw(&raw, folder, Utc::now());
            env.flags = flags;
            let ids = self
                .store
                .put_messages(vec![NewMessage { envelope: env, raw: Some(raw), body_text: Some(text) }])
                .map_err(store_err)?;
            ids.into_iter().next().ok_or_else(|| EngineError::Store("lote vacío".into()))?
        };
        cd.saved = Some(id);
        self.counts.clear();
        if self.view_folders().contains(&folder) {
            self.rebuild_rows();
        }
        Ok(id)
    }

    /// Borra un mensaje (borrador reemplazado o enviado): en local y, si ya estaba en el servidor,
    /// también allí.
    pub fn discard_message(&mut self, id: MessageId) {
        let account = self.store.get_envelope(id).ok().flatten().map(|e| e.account);
        if iuredesk_sync::ops::delete_message(&self.store, id).is_ok()
            && let Some(a) = account
        {
            self.kick(a);
        }
        self.envelopes.remove(&id);
        self.counts.clear();
    }

    /// Pide a la sincronización que replique ya la cola de la cuenta.
    pub(crate) fn kick(&self, account: AccountId) {
        if let Some(h) = &self.live.handle {
            h.kick(account);
        }
    }

    // ------------------------------------------------------------------ cuentas

    /// Cuentas para los ajustes.
    pub fn accounts_overview(&self) -> Vec<AccountRow> {
        let mut out = Vec::new();
        for a in &self.accounts {
            let aid = self.account_ids.get(&a.id).copied();
            out.push(AccountRow {
                id: a.id.clone(),
                aid,
                name: a.display_name.clone(),
                email: a.email.clone(),
                server: a.incoming.host.clone(),
                status: aid.and_then(|x| self.live.status.get(&x).cloned()),
                signature: self.signature_text(&a.email),
            });
        }
        for l in &self.labels {
            if !out.iter().any(|r| r.aid == Some(l.id)) {
                out.push(AccountRow {
                    id: String::new(),
                    aid: Some(l.id),
                    name: l.name.clone(),
                    email: l.email.clone(),
                    server: String::new(),
                    status: None,
                    signature: self.signature_text(&l.email),
                });
            }
        }
        out
    }

    /// Quita una cuenta: de la lista, del llavero y del almacén local (sus carpetas y mensajes).
    /// En el servidor no se toca nada.
    pub fn remove_account(&mut self, id: &str) -> Result<Outcome> {
        let account = self.accounts.iter().find(|a| a.id == id).cloned();
        let aid = self.account_ids.get(id).copied();
        let email = account.as_ref().map(|a| a.email.clone()).unwrap_or_default();
        if let Some(st) = &self.account_store {
            st.remove(id).map_err(|e| EngineError::Io(e.to_string()))?;
        }
        let secrets = Arc::clone(&self.secrets);
        let sid = id.to_string();
        // El llavero puede tardar (D-Bus): en segundo plano.
        self.bg.spawn_blocking(move || {
            let _ = secrets.delete_all(&sid);
            crate::JobOutput::Custom(Box::new(()))
        });
        if let Some(aid) = aid {
            let folders: Vec<_> = self.store.folders(aid).unwrap_or_default().into_iter().map(|f| f.id).collect();
            for op in self.store.peek_ops(10_000).unwrap_or_default().into_iter().filter(|o| o.account == aid) {
                let _ = self.store.ack_op(op.seq);
            }
            for f in &folders {
                self.store.delete_folder(*f).map_err(store_err)?;
            }
            let _ = iuredesk_sync::state::forget_account(&self.store, id, aid, &folders);
            self.store.delete_account_label(aid).map_err(store_err)?;
            self.live.status.remove(&aid);
        }
        self.account_ids.remove(id);
        self.selection.clear();
        self.focused = None;
        if let super::FolderSel::Folder(f) = self.folder
            && self.folder(f).is_some_and(|x| Some(x.account) == aid)
        {
            self.folder = super::FolderSel::default();
        }
        self.reload_accounts()?;
        self.restart_sync();
        self.refresh_transports();
        self.rebuild_rows();
        Ok(Outcome::Notice(Notice::success(
            format!("{email} was removed from IureDesk. Nothing was deleted on the server."),
            format!("Se quitó {email} de IureDesk. No se borró nada del servidor."),
        )))
    }

    /// Abre el asistente para volver a iniciar sesión en una cuenta (contraseña nueva o permiso
    /// OAuth caducado).
    pub fn reauth_account(&mut self, id: &str) {
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else { return };
        self.wizard_close();
        let mut w = crate::onboarding::Wizard {
            email: account.email.clone(),
            name: account.display_name.clone(),
            ..Default::default()
        };
        w.flow = Some(match account.incoming.auth {
            iuredesk_model::account::AuthMethod::OAuth2 { provider } => {
                w.oauth_ready = crate::onboarding::oauth_available(provider);
                crate::onboarding::Flow::OAuth(provider)
            }
            iuredesk_model::account::AuthMethod::AppPassword => crate::onboarding::Flow::AppPassword {
                help_url: match account.kind {
                    iuredesk_model::account::AccountKind::Gmail => "https://myaccount.google.com/apppasswords",
                    iuredesk_model::account::AccountKind::ICloud => "https://account.apple.com/account/manage",
                    _ => "https://account.microsoft.com/security",
                },
                steps: Text::new(
                    "Create a new app password and paste it below.",
                    "Crea una contraseña de app nueva y pégala aquí abajo.",
                ),
            },
            iuredesk_model::account::AuthMethod::Password => crate::onboarding::Flow::Password,
        });
        w.reserved = self.account_ids.get(id).copied();
        w.reauth = Some(account);
        w.step = crate::onboarding::WizardStep::Provider;
        self.wizard = Some(w);
    }
}
