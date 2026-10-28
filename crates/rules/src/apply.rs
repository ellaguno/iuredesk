//! Aplicar un [`Plan`] a un mensaje: local primero por la API de `sync` (mover, banderas,
//! etiquetas), y lo que necesita la app (enviar, avisar) como [`Effect`].
//!
//! Devuelve los pasos para deshacer ([`UndoStep`]) con el número de operación encolada, para que
//! «Deshacer» la cancele si aún no salió de la máquina.

use chrono::{DateTime, Utc};
use iuredesk_compose::{Draft, ForwardMode, Lang, ReplyOptions, RichBody, SignaturePlacement};
use iuredesk_model::account::Identity;
use iuredesk_model::{AccountId, Flags, Folder, FolderId, FolderRole, MessageId};
use iuredesk_store::Store;

use crate::book::SendLog;
use crate::error::Result;
use crate::eval::Plan;
use crate::facts::Facts;
use crate::rule::{Action, FolderRef};

/// Contexto para aplicar.
pub struct ApplyCtx<'a> {
    /// Almacén.
    pub store: &'a Store,
    /// Carpetas de todas las cuentas.
    pub folders: &'a [Folder],
    /// Mis direcciones (no se responde ni reenvía a uno mismo).
    pub my_addresses: &'a [String],
    /// Identidades para enviar, por cuenta.
    pub identities: &'a [(AccountId, Identity)],
    /// Ahora.
    pub now: DateTime<Utc>,
    /// Permitir acciones que envían correo (al llegar sí; «Aplicar a la carpeta» no, para no
    /// reenviar o responder correo antiguo en bloque).
    pub allow_send: bool,
    /// Idioma de los textos generados.
    pub spanish: bool,
}

/// Paso para deshacer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoStep {
    /// Volver a estas banderas.
    Flags {
        /// Mensaje.
        id: MessageId,
        /// Cuenta.
        account: AccountId,
        /// Banderas anteriores.
        flags: Flags,
        /// Operación encolada.
        seq: Option<u64>,
    },
    /// Volver a la carpeta anterior.
    Move {
        /// Mensaje.
        id: MessageId,
        /// Cuenta.
        account: AccountId,
        /// Carpeta anterior.
        folder: FolderId,
        /// Id remoto que tenía allí.
        remote: Option<String>,
        /// Operación encolada.
        seq: Option<u64>,
    },
    /// Volver a estas etiquetas (solo en local; se quita la añadida en el servidor).
    Keywords {
        /// Mensaje.
        id: MessageId,
        /// Etiquetas anteriores.
        keywords: Vec<String>,
        /// Etiqueta añadida.
        added: String,
    },
}

/// Lo que la app tiene que hacer.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Poner en la bandeja de salida.
    Send {
        /// Cuenta que envía.
        account: AccountId,
        /// Borrador.
        draft: Box<Draft>,
        /// Por qué (para el aviso).
        what: crate::Bi,
    },
    /// Avisar de este mensaje.
    Notify(MessageId),
    /// No avisar de este mensaje.
    Silence(MessageId),
}

/// Resultado de aplicar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// Pasos para deshacer, en orden de aplicación.
    pub undo: Vec<UndoStep>,
    /// Efectos para la app.
    pub effects: Vec<Effect>,
    /// El mensaje salió de su carpeta.
    pub moved: bool,
    /// Algo cambió.
    pub changed: bool,
}

fn resolve_folder(ctx: &ApplyCtx<'_>, target: &FolderRef, account: AccountId) -> Option<FolderId> {
    let mine = |f: &&Folder| f.account == account;
    ctx.folders
        .iter()
        .filter(mine)
        .find(|f| f.id == target.id)
        .or_else(|| ctx.folders.iter().filter(mine).find(|f| !target.path.is_empty() && f.remote_path == target.path))
        .or_else(|| ctx.folders.iter().filter(mine).find(|f| f.name.eq_ignore_ascii_case(&target.name)))
        .map(|f| f.id)
}

fn role_folder(ctx: &ApplyCtx<'_>, account: AccountId, role: FolderRole) -> Option<FolderId> {
    ctx.folders.iter().find(|f| f.account == account && f.role == role).map(|f| f.id)
}

fn identities_for(ctx: &ApplyCtx<'_>, account: AccountId) -> Vec<Identity> {
    let mut v: Vec<(AccountId, Identity)> = ctx.identities.to_vec();
    v.sort_by_key(|(a, _)| *a != account);
    v.into_iter().map(|(_, i)| i).collect()
}

fn reply_opts(ctx: &ApplyCtx<'_>, id: MessageId) -> ReplyOptions {
    ReplyOptions {
        lang: if ctx.spanish { Lang::Es } else { Lang::En },
        subject_style: Default::default(),
        tz: None,
        signature: None,
        placement: SignaturePlacement::default(),
        original_id: Some(id),
    }
}

/// Aplica el plan al mensaje de `facts`. `log` lleva la cuenta de lo enviado automáticamente
/// (protección contra bucles y «una vez por remitente»).
pub fn apply_plan(ctx: &ApplyCtx<'_>, facts: &Facts, plan: &Plan, log: &mut SendLog) -> Result<Applied> {
    let mut out = Applied::default();
    let id = facts.env.id;
    let account = facts.env.account;
    let Some(start) = ctx.store.get_envelope(id)? else { return Ok(out) };
    let mut flags = start.flags;
    let mut placed: Option<FolderId> = None;
    for (_, action) in &plan.actions {
        match action {
            // Las banderas se juntan y se aplican una sola vez (una operación, un deshacer).
            Action::Flag => flags.flagged = true,
            Action::MarkRead => flags.seen = true,
            Action::Label { name } => {
                let name = name.trim();
                let Some(mut env) = ctx.store.get_envelope(id)? else { continue };
                if name.is_empty() || env.keywords.iter().any(|k| k.eq_ignore_ascii_case(name)) {
                    continue;
                }
                let before = env.keywords.clone();
                env.keywords.push(name.to_string());
                put_keywords(ctx.store, env.clone(), true, name)?;
                out.undo.push(UndoStep::Keywords { id, keywords: before, added: name.to_string() });
                out.changed = true;
            }
            Action::MoveTo { folder } => {
                let Some(to) = resolve_folder(ctx, folder, account) else { continue };
                placed = Some(to);
            }
            Action::Delete => {
                placed = role_folder(ctx, account, FolderRole::Trash);
            }
            Action::Forward { to } | Action::Redirect { to } => {
                if !ctx.allow_send || !crate::vacation::may_forward(facts, to, ctx.my_addresses, log, ctx.now) {
                    continue;
                }
                let Ok(raw) = ctx.store.get_raw(id) else { continue };
                let ids = identities_for(ctx, account);
                let Ok(mut d) = iuredesk_compose::forward(&raw, &ids, ForwardMode::AsAttachment, &reply_opts(ctx, id))
                else {
                    continue;
                };
                let Ok(list) = iuredesk_compose::parse_address_list(to) else { continue };
                d.to = list;
                d.original = None;
                log.record_forward(facts.env.message_id.as_deref(), ctx.now);
                out.effects.push(Effect::Send {
                    account,
                    draft: Box::new(d),
                    what: crate::Bi::new(format!("Forwarded to {to}"), format!("Reenviado a {to}")),
                });
            }
            Action::AutoReply { subject, body, days } => {
                if !ctx.allow_send || !crate::vacation::may_auto_reply(facts, ctx.my_addresses, log, ctx.now, *days) {
                    continue;
                }
                let ids = identities_for(ctx, account);
                let Some(d) = crate::vacation::reply_draft(ctx.store, facts, &ids, subject, body, &reply_opts(ctx, id))
                else {
                    continue;
                };
                log.record_reply(&facts.sender(), ctx.now);
                let who = facts.sender();
                out.effects.push(Effect::Send {
                    account,
                    draft: Box::new(d),
                    what: crate::Bi::new(format!("Automatic reply to {who}"), format!("Respuesta automática a {who}")),
                });
            }
            Action::Notify => out.effects.push(Effect::Notify(id)),
            Action::Silence => out.effects.push(Effect::Silence(id)),
            Action::Stop => {}
        }
    }
    if flags != start.flags {
        let seq = iuredesk_sync::ops::set_flags(ctx.store, id, flags)?;
        out.undo.push(UndoStep::Flags { id, account, flags: start.flags, seq });
        out.changed = true;
    }
    if let Some(to) = placed {
        let env = ctx.store.get_envelope(id)?;
        if let Some(env) = env
            && env.folder != to
        {
            let seq = iuredesk_sync::ops::move_message(ctx.store, id, to)?;
            out.undo.push(UndoStep::Move { id, account, folder: env.folder, remote: env.remote_id.clone(), seq });
            out.moved = true;
            out.changed = true;
        }
    }
    Ok(out)
}

/// Guarda las etiquetas en local y encola el cambio en el servidor (palabra clave IMAP).
pub fn put_keywords(
    store: &Store,
    env: iuredesk_model::MessageEnvelope,
    add: bool,
    keyword: &str,
) -> Result<Option<u64>> {
    let id = env.id;
    let account = env.account;
    let folder = store.get_folder(env.folder)?;
    let uid = env.remote_id.as_deref().and_then(|r| r.parse::<u32>().ok()).filter(|u| *u > 0);
    store.put_messages(vec![iuredesk_store::NewMessage { envelope: env, raw: None, body_text: None }])?;
    let (Some(uid), Some(folder)) = (uid, folder) else { return Ok(None) };
    let kw = imap_keyword(keyword);
    let op = iuredesk_sync::ops::MailOp::Flags {
        msg: id,
        folder: folder.remote_path,
        uid,
        add: if add { vec![kw.clone()] } else { Vec::new() },
        remove: if add { Vec::new() } else { vec![kw] },
    };
    Ok(Some(store.enqueue_op(account, op.kind(), &op.encode()?)?))
}

/// Palabra clave IMAP válida (atom: sin espacios ni especiales).
pub fn imap_keyword(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '$' | '_' | '-' | '.') { c } else { '_' })
        .take(64)
        .collect();
    if s.is_empty() { "_".into() } else { s }
}

/// Deshace un paso de etiquetas (los de banderas y carpetas los deshace la app con su cola).
pub fn undo_keywords(store: &Store, id: MessageId, keywords: Vec<String>, added: &str) -> Result<()> {
    let Some(mut env) = store.get_envelope(id)? else { return Ok(()) };
    env.keywords = keywords;
    put_keywords(store, env, false, added)?;
    Ok(())
}

/// Texto de un borrador de respuesta automática.
pub(crate) fn plain(body: &str) -> RichBody {
    RichBody::from_plain(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_atoms() {
        assert_eq!(imap_keyword("Clientes VIP"), "Clientes_VIP");
        assert_eq!(imap_keyword("$Snoozed"), "$Snoozed");
        assert_eq!(imap_keyword("  "), "_");
    }
}
