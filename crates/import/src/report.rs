//! Progreso, avisos e informe final de una importación.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// Detalles de omisiones que se guardan en el informe (el recuento sigue aunque se llegue al tope).
pub const MAX_SKIPPED_DETAILS: usize = 5_000;
/// Avisos máximos en el informe.
pub const MAX_NOTICES: usize = 500;

/// Tipo de origen reconocido.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Directorio raíz de Thunderbird (con `profiles.ini`).
    Thunderbird,
    /// Un perfil de Thunderbird.
    ThunderbirdProfile,
    /// Libreta de Thunderbird (`abook.sqlite`, `history.sqlite`).
    ThunderbirdAddressBook,
    /// Apple Mail (`~/Library/Mail` o una carpeta `V*`).
    AppleMail,
    /// Buzón de Apple Mail (`Nombre.mbox/`, con `.emlx` o exportado con `mbox` dentro).
    AppleMailbox,
    /// Archivo de Outlook (`.pst`, `.ost`).
    OutlookPst,
    /// Archivo mbox (incluido el de Gmail Takeout).
    Mbox,
    /// Directorio Maildir (`cur/`, `new/`).
    Maildir,
    /// Mensaje suelto `.eml`.
    Eml,
    /// Mensaje suelto de Apple Mail `.emlx`.
    Emlx,
    /// Calendario o tareas `.ics`.
    Ics,
    /// Contactos `.vcf`.
    Vcf,
    /// Directorio cualquiera: se recorre buscando lo anterior (p. ej. un Google Takeout).
    Directory,
}

impl SourceKind {
    /// Nombre para la interfaz.
    pub fn label(self, lang: &str) -> &'static str {
        let en = lang.starts_with("en");
        match (self, en) {
            (Self::Thunderbird | Self::ThunderbirdProfile, _) => "Thunderbird",
            (Self::ThunderbirdAddressBook, true) => "Thunderbird address book",
            (Self::ThunderbirdAddressBook, false) => "Libreta de Thunderbird",
            (Self::AppleMail, _) => "Apple Mail",
            (Self::AppleMailbox, true) => "Apple Mail mailbox",
            (Self::AppleMailbox, false) => "Buzón de Apple Mail",
            (Self::OutlookPst, true) => "Outlook data file",
            (Self::OutlookPst, false) => "Archivo de datos de Outlook",
            (Self::Mbox, _) => "mbox",
            (Self::Maildir, _) => "Maildir",
            (Self::Eml, true) => "Email file (.eml)",
            (Self::Eml, false) => "Mensaje (.eml)",
            (Self::Emlx, true) => "Apple Mail message (.emlx)",
            (Self::Emlx, false) => "Mensaje de Apple Mail (.emlx)",
            (Self::Ics, true) => "Calendar (.ics)",
            (Self::Ics, false) => "Calendario (.ics)",
            (Self::Vcf, true) => "Contacts (.vcf)",
            (Self::Vcf, false) => "Contactos (.vcf)",
            (Self::Directory, true) => "Folder",
            (Self::Directory, false) => "Carpeta",
        }
    }
}

/// Por qué se omitió un elemento.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SkipReason {
    /// Supera el tamaño máximo configurado.
    TooLarge {
        /// Bytes.
        size: u64,
        /// Máximo.
        max: u64,
    },
    /// Vacío.
    Empty,
    /// No tiene forma de mensaje, contacto o evento.
    Malformed {
        /// Detalle técnico (para el registro).
        detail: String,
    },
    /// Marcado como borrado en el programa de origen (pendiente de compactar).
    DeletedInSource,
    /// Tipo de elemento que IureDesk aún no importa (lista de distribución, diario…).
    Unsupported {
        /// Qué era.
        what: String,
    },
    /// Error al leer el archivo.
    Io {
        /// Detalle.
        detail: String,
    },
    /// El lector del formato falló con este elemento dañado.
    Damaged,
    /// Se alcanzó un límite de seguridad (archivos, profundidad, elementos).
    LimitReached {
        /// Qué límite.
        what: String,
    },
}

impl SkipReason {
    /// Clave estable para agrupar en el informe.
    pub fn key(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "too_large",
            Self::Empty => "empty",
            Self::Malformed { .. } => "malformed",
            Self::DeletedInSource => "deleted_in_source",
            Self::Unsupported { .. } => "unsupported",
            Self::Io { .. } => "io",
            Self::Damaged => "damaged",
            Self::LimitReached { .. } => "limit_reached",
        }
    }

    /// Mensaje humano en `es` o `en`.
    pub fn human(&self, lang: &str) -> String {
        let en = lang.starts_with("en");
        match self {
            Self::TooLarge { size, max } => {
                let mb = |b: &u64| b / (1024 * 1024);
                if en {
                    format!("too large ({} MB; the limit is {} MB)", mb(size), mb(max))
                } else {
                    format!("demasiado grande ({} MB; el límite es {} MB)", mb(size), mb(max))
                }
            }
            Self::Empty => pick(en, "empty", "vacío"),
            Self::Malformed { .. } => pick(en, "unreadable or damaged", "ilegible o dañado"),
            Self::DeletedInSource => pick(
                en,
                "it was deleted in the original program (waiting to be compacted)",
                "estaba borrado en el programa original (pendiente de compactar)",
            ),
            Self::Unsupported { what } => {
                if en {
                    format!("{what} can't be imported yet")
                } else {
                    format!("todavía no se puede importar: {what}")
                }
            }
            Self::Io { .. } => pick(en, "the file couldn't be read", "no se pudo leer el archivo"),
            Self::Damaged => pick(en, "damaged item in the data file", "elemento dañado en el archivo de datos"),
            Self::LimitReached { what } => {
                if en {
                    format!("safety limit reached ({what})")
                } else {
                    format!("se alcanzó un límite de seguridad ({what})")
                }
            }
        }
    }
}

fn pick(en: bool, a: &str, b: &str) -> String {
    if en { a.to_string() } else { b.to_string() }
}

/// Elemento omitido.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Skipped {
    /// Dónde estaba: ruta y, si aplica, posición (`archivo.mbox#1532`, `archivo.pst › Bandeja › 17`).
    pub location: String,
    /// Motivo.
    pub reason: SkipReason,
}

/// Aviso que no impide importar pero que el usuario debe conocer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Notice {
    /// Carpeta IMAP de Thunderbird: es una copia sin conexión que puede estar incompleta.
    ImapOfflineCache {
        /// Ruta.
        path: PathBuf,
    },
    /// Calendario local de Thunderbird (`calendar-data/local.sqlite`): hay que exportarlo a `.ics`.
    ThunderbirdCalendarNeedsExport {
        /// Ruta.
        path: PathBuf,
    },
    /// Archivo OST que el lector no pudo abrir (OST de Outlook 2013+ usan otro formato de bloques).
    OstUnsupported {
        /// Ruta.
        path: PathBuf,
    },
    /// Citas repetidas de Outlook cuyas excepciones (repeticiones movidas) no se importaron.
    RecurrenceExceptionsDropped {
        /// Citas afectadas.
        count: u64,
    },
    /// Mensajes de Apple Mail descargados a medias cuyos adjuntos no se encontraron en disco.
    PartialMessages {
        /// Mensajes afectados.
        count: u64,
    },
    /// Elementos de un tipo que se ignora a propósito (p. ej. archivos `.msf` o índices).
    Ignored {
        /// Ruta.
        path: PathBuf,
        /// Qué es.
        what: String,
    },
}

impl Notice {
    /// Mensaje humano en `es` o `en`.
    pub fn human(&self, lang: &str) -> String {
        let en = lang.starts_with("en");
        match self {
            Self::ImapOfflineCache { path } => {
                if en {
                    format!(
                        "{}: this is Thunderbird's offline copy of an IMAP folder and may be incomplete. Add the account in IureDesk to get everything from the server.",
                        path.display()
                    )
                } else {
                    format!(
                        "{}: es la copia sin conexión de Thunderbird de una carpeta IMAP y puede estar incompleta. Agrega la cuenta en IureDesk para traer todo del servidor.",
                        path.display()
                    )
                }
            }
            Self::ThunderbirdCalendarNeedsExport { path } => {
                if en {
                    format!(
                        "{}: Thunderbird's local calendar. Export it from Thunderbird (Calendar › Export) as .ics and import that file.",
                        path.display()
                    )
                } else {
                    format!(
                        "{}: calendario local de Thunderbird. Expórtalo desde Thunderbird (Calendario › Exportar) como .ics e importa ese archivo.",
                        path.display()
                    )
                }
            }
            Self::OstUnsupported { path } => {
                if en {
                    format!(
                        "{}: this Outlook offline file can't be read. Add the account in IureDesk, or export a .pst from Outlook (File › Open & Export › Import/Export).",
                        path.display()
                    )
                } else {
                    format!(
                        "{}: este archivo sin conexión de Outlook no se puede leer. Agrega la cuenta en IureDesk o exporta un .pst desde Outlook (Archivo › Abrir y exportar › Importar o exportar).",
                        path.display()
                    )
                }
            }
            Self::RecurrenceExceptionsDropped { count } => {
                if en {
                    format!(
                        "{count} repeating appointments had individually moved or edited occurrences; those changes weren't imported."
                    )
                } else {
                    format!(
                        "{count} citas repetidas tenían repeticiones movidas o editadas una a una; esos cambios no se importaron."
                    )
                }
            }
            Self::PartialMessages { count } => {
                if en {
                    format!(
                        "{count} Apple Mail messages were only partly downloaded; their missing attachments weren't found on disk."
                    )
                } else {
                    format!(
                        "{count} mensajes de Apple Mail estaban descargados a medias; sus adjuntos no estaban en el disco."
                    )
                }
            }
            Self::Ignored { path, what } => {
                if en {
                    format!("{}: ignored ({what})", path.display())
                } else {
                    format!("{}: se ignora ({what})", path.display())
                }
            }
        }
    }
}

/// Recuentos de lo importado (también se envían como progreso).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Mensajes nuevos.
    pub messages: u64,
    /// Mensajes que ya estaban (por `Message-ID` + huella del contenido).
    pub duplicates: u64,
    /// Carpetas creadas.
    pub folders: u64,
    /// Contactos (nuevos o actualizados por `UID`).
    pub contacts: u64,
    /// Eventos (nuevos o actualizados por `UID`).
    pub events: u64,
    /// Tareas.
    pub tasks: u64,
    /// Notas.
    pub notes: u64,
    /// Elementos omitidos.
    pub skipped: u64,
}

/// Progreso de la importación.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Progress {
    /// Recuentos hasta ahora.
    pub counts: Counts,
    /// Bytes de origen procesados (aproximado).
    pub bytes_done: u64,
    /// Bytes de origen totales (estimado al empezar; 0 si no se sabe).
    pub bytes_total: u64,
    /// Lo que se está leyendo ahora (cuenta › carpeta, o archivo).
    pub current: String,
}

impl Progress {
    /// Fracción completada (0–1), si se conoce el total.
    pub fn fraction(&self) -> Option<f32> {
        if self.bytes_total == 0 {
            return None;
        }
        Some((self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0) as f32)
    }
}

/// Evento para la interfaz (se emite desde el hilo que importa).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum ImportEvent {
    /// Empieza un origen.
    SourceStarted {
        /// Tipo.
        kind: SourceKind,
        /// Ruta.
        path: PathBuf,
    },
    /// Progreso (como mucho unas pocas veces por segundo).
    Progress(Progress),
    /// Se omitió un elemento.
    Skipped(Skipped),
    /// Aviso.
    Notice(Notice),
}

/// Informe final.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ImportReport {
    /// Orígenes procesados.
    pub sources: Vec<(SourceKind, PathBuf)>,
    /// Cuentas locales donde quedó el correo (nombre visible).
    pub accounts: Vec<String>,
    /// Recuentos.
    pub counts: Counts,
    /// Omitidos por motivo (clave de [`SkipReason::key`]).
    pub skipped_by_reason: BTreeMap<String, u64>,
    /// Detalle de omisiones (como mucho [`MAX_SKIPPED_DETAILS`]).
    pub skipped: Vec<Skipped>,
    /// Avisos (como mucho [`MAX_NOTICES`]).
    pub notices: Vec<Notice>,
    /// Bytes de origen leídos.
    pub bytes_read: u64,
    /// La importación se canceló; volver a lanzarla continúa donde se quedó.
    pub cancelled: bool,
    /// Se reanudó una importación anterior (se saltaron archivos ya terminados).
    pub resumed: bool,
}

impl ImportReport {
    /// Resumen de una línea para la interfaz.
    pub fn summary(&self, lang: &str) -> String {
        let c = &self.counts;
        if lang.starts_with("en") {
            format!(
                "{} messages ({} already there), {} contacts, {} events, {} tasks, {} notes imported; {} skipped.",
                c.messages, c.duplicates, c.contacts, c.events, c.tasks, c.notes, c.skipped
            )
        } else {
            format!(
                "Importados {} mensajes ({} ya estaban), {} contactos, {} eventos, {} tareas y {} notas; {} omitidos.",
                c.messages, c.duplicates, c.contacts, c.events, c.tasks, c.notes, c.skipped
            )
        }
    }

    pub(crate) fn add_skip(&mut self, s: Skipped) {
        self.counts.skipped = self.counts.skipped.saturating_add(1);
        let n = self.skipped_by_reason.entry(s.reason.key().to_string()).or_insert(0);
        *n = n.saturating_add(1);
        if self.skipped.len() < MAX_SKIPPED_DETAILS {
            self.skipped.push(s);
        }
    }

    pub(crate) fn add_notice(&mut self, n: Notice) -> bool {
        if self.notices.len() < MAX_NOTICES && !self.notices.contains(&n) {
            self.notices.push(n);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_details_are_capped_but_counted() {
        let mut r = ImportReport::default();
        for i in 0..(MAX_SKIPPED_DETAILS + 10) {
            r.add_skip(Skipped { location: format!("x#{i}"), reason: SkipReason::Empty });
        }
        assert_eq!(r.skipped.len(), MAX_SKIPPED_DETAILS);
        assert_eq!(r.counts.skipped, (MAX_SKIPPED_DETAILS + 10) as u64);
        assert_eq!(r.skipped_by_reason.get("empty"), Some(&((MAX_SKIPPED_DETAILS + 10) as u64)));
        assert!(r.summary("es").contains("omitidos"));
        assert!(r.summary("en").contains("skipped"));
    }

    #[test]
    fn human_messages_in_both_languages() {
        let r = SkipReason::TooLarge { size: 300 * 1024 * 1024, max: 128 * 1024 * 1024 };
        assert!(r.human("es").contains("300 MB"));
        assert!(r.human("en").contains("limit"));
        let n = Notice::RecurrenceExceptionsDropped { count: 3 };
        assert!(n.human("es").starts_with('3'));
    }
}
