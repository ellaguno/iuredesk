//! Error del redactor. `Display` en español para registros; [`ComposeError::message`] para el
//! usuario, en su idioma.

use crate::address::AddressError;
use crate::lang::Lang;

/// Resultado del crate.
pub type Result<T, E = ComposeError> = std::result::Result<T, E>;

/// Error de cualquier operación del redactor (salvo el envío, que usa
/// [`SendError`](crate::transport::SendError)).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    /// Una dirección no es válida.
    #[error("dirección no válida: {0}")]
    Address(#[from] AddressError),
    /// El borrador no tiene destinatarios.
    #[error("el borrador no tiene destinatarios")]
    NoRecipients,
    /// Hay demasiados destinatarios.
    #[error("demasiados destinatarios ({count}; máximo {max})")]
    TooManyRecipients { count: usize, max: usize },
    /// La identidad de envío no tiene una dirección válida.
    #[error("la identidad de envío no es válida: {0}")]
    Identity(String),
    /// No se pudo leer un adjunto.
    #[error("adjunto «{name}»: {reason}")]
    Attachment { name: String, reason: String },
    /// El mensaje supera el máximo absoluto que se construye.
    #[error("el mensaje es demasiado grande: {size} bytes (máximo {max})")]
    TooLarge { size: u64, max: u64 },
    /// El correo original no se pudo leer.
    #[error("no se pudo leer el correo original")]
    Unparseable,
    /// La invitación de calendario no es un iCalendar válido.
    #[error("invitación de calendario no válida: {0}")]
    Calendar(String),
    /// No existe la plantilla, firma o entrada pedida.
    #[error("no existe {what} «{name}»")]
    NotFound { what: &'static str, name: String },
    /// Argumento no válido.
    #[error("dato no válido: {0}")]
    Invalid(String),
    /// La entrada de la bandeja de salida ya no se puede cancelar.
    #[error("el mensaje ya se está enviando o ya se envió")]
    NotCancellable,
    /// Fallo del almacén local.
    #[error("almacén: {0}")]
    Store(String),
    /// Fallo al construir el MIME.
    #[error("no se pudo construir el mensaje: {0}")]
    Build(String),
    /// No se pudo firmar o cifrar (F5.4); mensaje humano en los dos idiomas.
    #[error("correo seguro: {es}")]
    Security { es: String, en: String },
}

impl From<iuredesk_store::StoreError> for ComposeError {
    fn from(e: iuredesk_store::StoreError) -> Self {
        ComposeError::Store(e.to_string())
    }
}

impl From<serde_json::Error> for ComposeError {
    fn from(e: serde_json::Error) -> Self {
        ComposeError::Store(format!("registro dañado: {e}"))
    }
}

impl ComposeError {
    /// Mensaje para mostrar al usuario.
    pub fn message(&self, lang: Lang) -> String {
        match self {
            ComposeError::Address(e) => e.message(lang),
            ComposeError::NoRecipients => {
                lang.pick("Add at least one recipient.", "Agrega al menos un destinatario.").to_string()
            }
            ComposeError::TooManyRecipients { max, .. } => match lang {
                Lang::Es => format!("Hay demasiados destinatarios. El máximo es {max}."),
                Lang::En => format!("There are too many recipients. The maximum is {max}."),
            },
            ComposeError::Identity(addr) => match lang {
                Lang::Es => format!("La dirección de envío «{addr}» no es válida. Revisa la identidad de la cuenta."),
                Lang::En => format!("The sender address “{addr}” is not valid. Check the account identity."),
            },
            ComposeError::Attachment { name, .. } => match lang {
                Lang::Es => format!("No se pudo leer el adjunto «{name}». ¿Se movió o se borró el archivo?"),
                Lang::En => format!("Couldn't read the attachment “{name}”. Was the file moved or deleted?"),
            },
            ComposeError::TooLarge { .. } => lang
                .pick(
                    "The message is too large to send. Share big files as a link instead.",
                    "El mensaje es demasiado grande para enviarse. Comparte los archivos grandes como enlace.",
                )
                .to_string(),
            ComposeError::Unparseable => {
                lang.pick("The original message couldn't be read.", "No se pudo leer el correo original.").to_string()
            }
            ComposeError::Calendar(_) => {
                lang.pick("The calendar invitation is damaged.", "La invitación de calendario está dañada.").to_string()
            }
            ComposeError::NotFound { name, .. } => match lang {
                Lang::Es => format!("No se encontró «{name}»."),
                Lang::En => format!("“{name}” was not found."),
            },
            ComposeError::Invalid(_) | ComposeError::Build(_) => {
                lang.pick("The message couldn't be prepared.", "No se pudo preparar el mensaje.").to_string()
            }
            ComposeError::NotCancellable => lang
                .pick(
                    "Too late to undo: the message is already on its way.",
                    "Ya no se puede deshacer: el mensaje ya va en camino.",
                )
                .to_string(),
            ComposeError::Security { es, en } => match lang {
                Lang::Es => es.clone(),
                Lang::En => en.clone(),
            },
            ComposeError::Store(_) => lang
                .pick("Couldn't save to the local mail store.", "No se pudo guardar en el almacén local.")
                .to_string(),
        }
    }
}
