//! Resultado de abrir un mensaje protegido: lo que muestra la insignia de seguridad del panel de
//! lectura. Tres estados simples para el usuario (Verificado / Clave nueva / No coincide) más los
//! fallos, con textos en lenguaje humano en los dos idiomas.

use serde::{Deserialize, Serialize};

/// Tecnología con la que viene protegido el mensaje.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Technology {
    OpenPgp,
    Smime,
}

impl Technology {
    pub fn label(self) -> &'static str {
        match self {
            Technology::OpenPgp => "OpenPGP",
            Technology::Smime => "S/MIME",
        }
    }
}

/// Estado de la firma, de cara al usuario.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureState {
    /// Firma válida con la clave que ya se conocía del remitente (OpenPGP) o con un certificado
    /// vigente emitido por una autoridad de confianza (S/MIME).
    Verified,
    /// Firma válida, pero es la primera vez que se ve esta clave para el remitente (OpenPGP), o el
    /// emisor del certificado no es de confianza (S/MIME).
    NewKey,
    /// La firma es válida pero la clave no es la que se conocía del remitente, o el firmante no es
    /// quien dice el campo «De».
    Mismatch,
    /// No se puede comprobar: falta la clave pública del firmante.
    MissingKey,
    /// La firma no corresponde al contenido (mensaje alterado) o está dañada.
    Invalid(String),
}

/// Firma de un mensaje.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureInfo {
    pub state: SignatureState,
    /// Quién firma («Ana <ana@x.com>» o el sujeto del certificado).
    pub signer: Option<String>,
    /// Dirección del firmante según su clave o certificado.
    pub signer_addr: Option<String>,
    /// Huella (OpenPGP) o número de serie y emisor (S/MIME).
    pub fingerprint: Option<String>,
    /// Emisor del certificado (S/MIME).
    pub issuer: Option<String>,
    /// Fin de la vigencia del certificado (S/MIME), RFC 3339.
    pub valid_until: Option<String>,
    /// Detalle técnico del motivo (para «Detalles»).
    pub detail: Option<String>,
}

impl SignatureInfo {
    pub fn new(state: SignatureState) -> Self {
        SignatureInfo {
            state,
            signer: None,
            signer_addr: None,
            fingerprint: None,
            issuer: None,
            valid_until: None,
            detail: None,
        }
    }
}

/// Informe de seguridad de un mensaje.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityReport {
    pub technology: Technology,
    /// El mensaje venía cifrado.
    pub encrypted: bool,
    /// Venía cifrado y no se pudo descifrar (motivo humano en español / inglés).
    pub decrypt_error: Option<(String, String)>,
    /// Firma, si la hay.
    pub signature: Option<SignatureInfo>,
    /// Protección en línea (bloque `-----BEGIN PGP …`), menos fiable que PGP/MIME: no cubre
    /// adjuntos ni cabeceras.
    pub inline: bool,
}

/// Tono de la insignia.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Neutral,
    Warning,
    Bad,
}

impl SecurityReport {
    pub fn new(technology: Technology) -> Self {
        SecurityReport { technology, encrypted: false, decrypt_error: None, signature: None, inline: false }
    }

    /// Tono global: lo peor de cifrado y firma.
    pub fn tone(&self) -> Tone {
        if self.decrypt_error.is_some() {
            return Tone::Bad;
        }
        match self.signature.as_ref().map(|s| &s.state) {
            Some(SignatureState::Verified) => Tone::Good,
            Some(SignatureState::NewKey | SignatureState::MissingKey) => Tone::Neutral,
            Some(SignatureState::Mismatch) => Tone::Warning,
            Some(SignatureState::Invalid(_)) => Tone::Bad,
            None if self.encrypted => Tone::Good,
            None => Tone::Neutral,
        }
    }

    /// Texto corto de la insignia (`es` = español).
    pub fn title(&self, es: bool) -> String {
        let t = |en: &str, sp: &str| if es { sp.to_string() } else { en.to_string() };
        if self.decrypt_error.is_some() {
            return t("Can't decrypt", "No se pudo descifrar");
        }
        let sig = self.signature.as_ref().map(|s| match &s.state {
            SignatureState::Verified => t("Signed · Verified", "Firmado · Verificado"),
            SignatureState::NewKey => match self.technology {
                Technology::OpenPgp => t("Signed · New key", "Firmado · Clave nueva"),
                Technology::Smime => t("Signed · Unknown issuer", "Firmado · Emisor no reconocido"),
            },
            SignatureState::Mismatch => t("Signed · Doesn't match", "Firmado · No coincide"),
            SignatureState::MissingKey => t("Signed · Can't check", "Firmado · Sin comprobar"),
            SignatureState::Invalid(_) => t("Verification failed", "Verificación fallida"),
        });
        match (self.encrypted, sig) {
            (true, Some(s)) => format!("{} · {s}", t("Encrypted", "Cifrado")),
            (true, None) => t("Encrypted", "Cifrado"),
            (false, Some(s)) => s,
            (false, None) => t("Protected", "Protegido"),
        }
    }

    /// Explicación en lenguaje humano para el panel de detalles (líneas).
    pub fn details(&self, es: bool) -> Vec<String> {
        let t = |en: String, sp: String| if es { sp } else { en };
        let mut out = Vec::new();
        let tech = self.technology.label();
        if let Some((sp, en)) = &self.decrypt_error {
            out.push(t(en.clone(), sp.clone()));
        } else if self.encrypted {
            out.push(t(
                format!("This message was encrypted with {tech}: only you and the other recipients can read it."),
                format!("Este mensaje llegó cifrado con {tech}: solo tú y los demás destinatarios pueden leerlo."),
            ));
        }
        if let Some(s) = &self.signature {
            let who = s.signer.clone().unwrap_or_else(|| t("the sender".into(), "el remitente".into()));
            out.push(match &s.state {
                SignatureState::Verified => t(
                    format!("Signed by {who}. The signature is valid and the content hasn't changed."),
                    format!("Firmado por {who}. La firma es válida y el contenido no se ha modificado."),
                ),
                SignatureState::NewKey => match self.technology {
                    Technology::OpenPgp => t(
                        format!("Signed by {who} with a key seen for the first time. If in doubt, confirm the fingerprint with them."),
                        format!("Firmado por {who} con una clave que se ve por primera vez. Si tienes dudas, confirma la huella con esa persona."),
                    ),
                    Technology::Smime => t(
                        format!("Signed by {who}. The signature is valid, but the certificate wasn't issued by an authority this computer trusts."),
                        format!("Firmado por {who}. La firma es válida, pero el certificado no lo emitió una autoridad en la que confíe este equipo."),
                    ),
                },
                SignatureState::Mismatch => t(
                    format!("Careful: the signature is from {who}, but it doesn't match the key known for the sender. It could be an impersonation."),
                    format!("Cuidado: la firma es de {who}, pero no coincide con la clave conocida del remitente. Podría ser una suplantación."),
                ),
                SignatureState::MissingKey => t(
                    "The message is signed, but the sender's key isn't available to check it.".into(),
                    "El mensaje está firmado, pero no se tiene la clave del remitente para comprobarlo.".into(),
                ),
                SignatureState::Invalid(_) => t(
                    "The signature doesn't match the content: the message may have been altered on the way.".into(),
                    "La firma no corresponde al contenido: el mensaje pudo alterarse por el camino.".into(),
                ),
            });
            if let Some(f) = &s.fingerprint {
                out.push(t(format!("Fingerprint: {f}"), format!("Huella: {f}")));
            }
            if let Some(i) = &s.issuer {
                out.push(t(format!("Issued by: {i}"), format!("Emitido por: {i}")));
            }
            if let Some(v) = &s.valid_until {
                out.push(t(format!("Valid until: {v}"), format!("Vigente hasta: {v}")));
            }
            if let Some(d) = &s.detail {
                out.push(d.clone());
            }
        }
        if self.inline {
            out.push(t(
                "Inline protection: attachments and the subject are not covered.".into(),
                "Protección en línea: no cubre los adjuntos ni el asunto.".into(),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        let mut r = SecurityReport::new(Technology::OpenPgp);
        r.encrypted = true;
        r.signature = Some(SignatureInfo::new(SignatureState::Verified));
        assert_eq!(r.title(true), "Cifrado · Firmado · Verificado");
        assert_eq!(r.tone(), Tone::Good);
        r.signature = Some(SignatureInfo::new(SignatureState::Mismatch));
        assert_eq!(r.tone(), Tone::Warning);
        assert!(r.details(true).iter().any(|l| l.contains("suplantación")));
        r.decrypt_error = Some(("x".into(), "y".into()));
        assert_eq!(r.title(false), "Can't decrypt");
    }
}
