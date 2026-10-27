//! Error del correo seguro. `Display` en español para registros; [`CryptoError::message`] da el
//! texto para el usuario en su idioma, en lenguaje humano.

/// Resultado del crate.
pub type Result<T, E = CryptoError> = std::result::Result<T, E>;

/// Error de cualquier operación criptográfica.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// No hay clave propia (OpenPGP) ni certificado (S/MIME) para la dirección del remitente.
    #[error("no hay clave propia para {0}")]
    NoOwnKey(String),
    /// Faltan claves de algunos destinatarios para cifrar.
    #[error("faltan claves de: {}", .0.join(", "))]
    MissingRecipientKeys(Vec<String>),
    /// La clave o el certificado no se pudo leer.
    #[error("clave no válida: {0}")]
    BadKey(String),
    /// Los datos (mensaje, firma, estructura CMS) están mal formados.
    #[error("datos mal formados: {0}")]
    Malformed(String),
    /// Algoritmo que no se admite.
    #[error("algoritmo no admitido: {0}")]
    Unsupported(String),
    /// Contraseña del archivo `.p12` o de la clave incorrecta.
    #[error("contraseña incorrecta")]
    BadPassword,
    /// El llavero del sistema falló.
    #[error("llavero: {0}")]
    Vault(String),
    /// Error de disco.
    #[error("archivo: {0}")]
    Io(String),
    /// Error de red (WKD).
    #[error("red: {0}")]
    Network(String),
    /// Demasiado grande.
    #[error("demasiado grande ({0} bytes)")]
    TooLarge(usize),
    /// Fallo interno de una biblioteca criptográfica (pánico atrapado o error inesperado).
    #[error("fallo interno: {0}")]
    Internal(String),
}

impl CryptoError {
    /// Mensaje para el usuario (`es` = español).
    pub fn message(&self, es: bool) -> String {
        let (en, sp) = match self {
            CryptoError::NoOwnKey(a) => (
                format!("You don't have a signing key for {a}. Create or import one in Settings › Security."),
                format!("No tienes una clave para firmar como {a}. Crea o importa una en Ajustes › Seguridad."),
            ),
            CryptoError::MissingRecipientKeys(v) => (
                format!(
                    "The message can't be encrypted: there is no key for {}. Send it without encryption or ask them for their key.",
                    v.join(", ")
                ),
                format!(
                    "No se puede cifrar: falta la clave de {}. Envíalo sin cifrar o pídeles su clave.",
                    v.join(", ")
                ),
            ),
            CryptoError::BadKey(e) => {
                (format!("The key or certificate is not valid ({e})."), format!("La clave o el certificado no es válido ({e})."))
            }
            CryptoError::Malformed(e) => {
                (format!("The protected message is damaged ({e})."), format!("El mensaje protegido está dañado ({e})."))
            }
            CryptoError::Unsupported(e) => (
                format!("This message uses a method IureDesk doesn't support yet ({e})."),
                format!("Este mensaje usa un método que IureDesk aún no admite ({e})."),
            ),
            CryptoError::BadPassword => ("The password is not correct.".into(), "La contraseña no es correcta.".into()),
            CryptoError::Vault(e) => (
                format!("The system keyring couldn't be used ({e})."),
                format!("No se pudo usar el llavero del sistema ({e})."),
            ),
            CryptoError::Io(e) => (format!("A key file couldn't be read or saved ({e})."), format!("No se pudo leer o guardar un archivo de claves ({e}).")),
            CryptoError::Network(e) => (
                format!("The key server couldn't be reached ({e})."),
                format!("No se pudo consultar el servidor de claves ({e})."),
            ),
            CryptoError::TooLarge(n) => (
                format!("The message is too large to protect ({n} bytes)."),
                format!("El mensaje es demasiado grande para protegerlo ({n} bytes)."),
            ),
            CryptoError::Internal(e) => (
                format!("Something went wrong while protecting the message ({e})."),
                format!("Algo falló al proteger el mensaje ({e})."),
            ),
        };
        if es { sp } else { en }
    }
}

impl From<pgp::errors::Error> for CryptoError {
    fn from(e: pgp::errors::Error) -> Self {
        CryptoError::Malformed(e.to_string())
    }
}

impl From<der::Error> for CryptoError {
    fn from(e: der::Error) -> Self {
        CryptoError::Malformed(e.to_string())
    }
}

impl From<std::io::Error> for CryptoError {
    fn from(e: std::io::Error) -> Self {
        CryptoError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for CryptoError {
    fn from(e: serde_json::Error) -> Self {
        CryptoError::Io(e.to_string())
    }
}

/// Ejecuta `f` atrapando cualquier pánico de una biblioteca de terceros (nunca fallar).
pub(crate) fn guard<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(CryptoError::Internal(format!("{what}: pánico atrapado"))),
    }
}
