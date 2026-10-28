//! Gancho de correo seguro (F5.4): el borrador pide «Firmar» / «Cifrar» y, al construir el
//! mensaje para enviarlo, un [`MessageSealer`] (lo implementa `engine` con `iuredesk-crypto`)
//! transforma los bytes. El redactor no sabe nada de OpenPGP ni de S/MIME.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{ComposeError, Result};
use crate::mime::{BuildOptions, BuiltMessage, ContentResolver, build_message};

/// Protección pedida para un borrador.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityRequest {
    #[serde(default)]
    pub sign: bool,
    #[serde(default)]
    pub encrypt: bool,
}

/// Quien firma / cifra el mensaje construido. Se llama con cada mensaje (también sin protección
/// pedida, para agregar la cabecera `Autocrypt`). Devuelve los bytes nuevos o un mensaje humano
/// (español, inglés) que impide el envío.
pub trait MessageSealer: Send + Sync {
    fn seal(
        &self,
        raw: &[u8],
        recipients: &[String],
        req: SecurityRequest,
    ) -> std::result::Result<Vec<u8>, (String, String)>;
}

/// Construye el mensaje y, si hay sellador, lo protege.
pub fn build_sealed(
    draft: &crate::Draft,
    resolver: &dyn ContentResolver,
    opts: &BuildOptions,
    sealer: Option<&Arc<dyn MessageSealer>>,
) -> Result<BuiltMessage> {
    let mut built = build_message(draft, resolver, opts)?;
    if let Some(s) = sealer {
        built.raw = s
            .seal(&built.raw, &built.recipients, draft.security)
            .map_err(|(es, en)| ComposeError::Security { es, en })?;
    } else if draft.security.sign || draft.security.encrypt {
        return Err(ComposeError::Security {
            es: "El correo seguro no está disponible en esta sesión.".into(),
            en: "Secure mail isn't available in this session.".into(),
        });
    }
    Ok(built)
}
