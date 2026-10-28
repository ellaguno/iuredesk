//! «Todo es un comando»: id estable, etiquetas en/es, icono, grupo y atajo por esquema. Menús,
//! barra de acciones, atajos y paleta despachan por id con [`crate::Session::run`]; las llamadas
//! programáticas nunca abren diálogos (devuelven un [`crate::Outcome`] que la UI interpreta).

use crate::prefs::ShortcutScheme;

/// Una combinación de teclas. `key` es el nombre de tecla de egui (`"R"`, `"Delete"`, `"Num1"`,
/// `"Slash"`…), así el motor no depende de egui y la UI la traduce con `Key::from_name`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Ctrl en Linux/Windows, ⌘ en macOS.
    pub command: bool,
    /// Ctrl además de ⌘ (solo tiene sentido en macOS; fuera de él equivale a `command`).
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: &'static str,
}

impl Chord {
    /// Tecla sola (atajos de una letra estilo Gmail).
    pub const fn key(key: &'static str) -> Chord {
        Chord { command: false, ctrl: false, shift: false, alt: false, key }
    }
    pub const fn cmd(key: &'static str) -> Chord {
        Chord { command: true, ctrl: false, shift: false, alt: false, key }
    }
    pub const fn cmd_shift(key: &'static str) -> Chord {
        Chord { command: true, ctrl: false, shift: true, alt: false, key }
    }
    pub const fn cmd_alt(key: &'static str) -> Chord {
        Chord { command: true, ctrl: false, shift: false, alt: true, key }
    }
    pub const fn cmd_ctrl(key: &'static str) -> Chord {
        Chord { command: true, ctrl: true, shift: false, alt: false, key }
    }
    pub const fn shift(key: &'static str) -> Chord {
        Chord { command: false, ctrl: false, shift: true, alt: false, key }
    }

    /// ¿Lleva modificadores? Los atajos de una tecla no se disparan mientras se escribe.
    pub fn has_modifier(&self) -> bool {
        self.command || self.ctrl || self.alt
    }

    /// Texto para mostrar: `Ctrl+Shift+R` (o `⌘⇧R` en macOS).
    pub fn label(&self, mac: bool) -> String {
        let key = key_label(self.key, self.shift);
        let shift_in_key = self.shift && key != key_label(self.key, false);
        if mac {
            let mut s = String::new();
            if self.ctrl {
                s.push('⌃');
            }
            if self.alt {
                s.push('⌥');
            }
            if self.shift && !shift_in_key {
                s.push('⇧');
            }
            if self.command {
                s.push('⌘');
            }
            s.push_str(&key);
            s
        } else {
            let mut parts: Vec<String> = Vec::new();
            if self.command || self.ctrl {
                parts.push("Ctrl".into());
            }
            if self.alt {
                parts.push("Alt".into());
            }
            if self.shift && !shift_in_key {
                parts.push("Shift".into());
            }
            parts.push(key);
            parts.join("+")
        }
    }
}

/// Nombre visible de una tecla de egui.
fn key_label(key: &str, shift: bool) -> String {
    match key {
        "Num0" | "Num1" | "Num2" | "Num3" | "Num4" | "Num5" | "Num6" | "Num7" | "Num8" | "Num9" => {
            let d = key.trim_start_matches("Num");
            if shift && d == "3" {
                return "#".into();
            }
            d.to_string()
        }
        "Delete" => crate::lang::pick("Del", "Supr").into(),
        "Backspace" => crate::lang::pick("Backspace", "Retroceso").into(),
        "Insert" => crate::lang::pick("Ins", "Insert").into(),
        "Enter" => "Enter".into(),
        "Escape" => "Esc".into(),
        "Slash" => "/".into(),
        "Comma" => ",".into(),
        "Period" => ".".into(),
        other => other.to_string(),
    }
}

/// Grupo del comando (sugerencia en la paleta).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Mail,
    Calendar,
    Navigate,
    View,
    Assistant,
    App,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Mail => crate::lang::pick("Mail", "Correo"),
            Group::Calendar => crate::lang::pick("Calendar", "Calendario"),
            Group::Navigate => crate::lang::pick("Go to", "Ir a"),
            Group::View => crate::lang::pick("View", "Vista"),
            Group::Assistant => crate::lang::pick("Assistant", "Asistente"),
            Group::App => "IureDesk",
        }
    }
}

/// Definición de un comando.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: &'static str,
    pub en: &'static str,
    pub es: &'static str,
    /// Nombre del icono del ui-kit (`Icon::named`).
    pub icon: &'static str,
    pub group: Group,
    /// Palabras extra para la paleta (sinónimos en los dos idiomas).
    pub keywords: &'static str,
}

impl CommandSpec {
    /// Etiqueta en el idioma actual.
    pub fn label(&self) -> &'static str {
        crate::lang::pick(self.en, self.es)
    }
}

const fn c(
    id: &'static str,
    en: &'static str,
    es: &'static str,
    icon: &'static str,
    group: Group,
    keywords: &'static str,
) -> CommandSpec {
    CommandSpec { id, en, es, icon, group, keywords }
}

/// Todos los comandos.
pub const COMMANDS: &[CommandSpec] = &[
    c("mail.new", "New message", "Nuevo mensaje", "mail", Group::Mail, "compose write redactar escribir correo"),
    c("mail.reply", "Reply", "Responder", "reply", Group::Mail, "answer contestar"),
    c("mail.reply_all", "Reply all", "Responder a todos", "reply_all", Group::Mail, "answer all contestar todos"),
    c("mail.forward", "Forward", "Reenviar", "forward", Group::Mail, "fwd reenvio"),
    c("mail.archive", "Archive", "Archivar", "archive", Group::Mail, "archivo done listo"),
    c("mail.delete", "Delete", "Eliminar", "trash", Group::Mail, "trash remove borrar papelera"),
    c(
        "mail.toggle_read",
        "Mark as read / unread",
        "Marcar como leído / no leído",
        "check",
        Group::Mail,
        "read unread leido",
    ),
    c(
        "mail.toggle_flag",
        "Flag / unflag",
        "Marcar / quitar seguimiento",
        "flag",
        Group::Mail,
        "star follow up destacar bandera",
    ),
    c(
        "contacts.delete",
        "Delete contacts",
        "Eliminar contactos",
        "trash",
        Group::App,
        "contacts people remove borrar eliminar personas",
    ),
    c("mail.move", "Move to folder…", "Mover a carpeta…", "folder", Group::Mail, "move file mover carpeta"),
    c("mail.search", "Search mail", "Buscar en el correo", "search", Group::Mail, "find buscar encontrar"),
    // F5.3
    c("mail.snooze", "Snooze…", "Posponer…", "clock", Group::Mail, "snooze later remind posponer despues recordar"),
    c("mail.unsnooze", "Unsnooze", "Quitar posposición", "undo", Group::Mail, "unsnooze despertar"),
    c(
        "mail.followup",
        "Remind me if nobody replies…",
        "Recordarme si no responden…",
        "bell",
        Group::Mail,
        "follow up reminder seguimiento recordatorio respuesta",
    ),
    c(
        "mail.rule_from_message",
        "Create rule from this message…",
        "Crear regla a partir de este correo…",
        "filter",
        Group::Mail,
        "rule filter regla filtro",
    ),
    c(
        "mail.apply_rules",
        "Apply rules to this folder",
        "Aplicar reglas a esta carpeta",
        "filter",
        Group::Mail,
        "run rules ejecutar reglas filtros",
    ),
    c(
        "mail.unsubscribe",
        "Unsubscribe…",
        "Cancelar suscripción…",
        "close",
        Group::Mail,
        "unsubscribe baja newsletter boletin",
    ),
    c(
        "mail.category",
        "Inbox category…",
        "Categoría de la bandeja…",
        "inbox",
        Group::Mail,
        "category primary promotions social updates forums categoria principal promociones notificaciones foros",
    ),
    c(
        "mail.always_category",
        "Always move sender to…",
        "Mover siempre al remitente a…",
        "inbox",
        Group::Mail,
        "always category siempre categoria",
    ),
    c(
        "mail.iure_only",
        "Only Iurefficient messages",
        "Solo mensajes de Iurefficient",
        "sparkle",
        Group::Mail,
        "iurefficient crm case lead filter caso filtro",
    ),
    c("app.labels", "Labels and colors…", "Etiquetas y colores…", "tag", Group::App, "labels colors etiquetas colores"),
    c("app.rules", "Rules…", "Reglas…", "filter", Group::App, "rules filters sieve reglas filtros"),
    c(
        "app.vacation",
        "Automatic reply…",
        "Respuesta automática…",
        "mail",
        Group::App,
        "vacation out of office away vacaciones ausente fuera oficina",
    ),
    c(
        "mail.toggle_conversations",
        "Group by conversation",
        "Agrupar por conversación",
        "list",
        Group::View,
        "threads conversation hilos",
    ),
    c("cal.new_event", "New event", "Nuevo evento", "plus", Group::Calendar, "appointment meeting cita reunion evento"),
    c("cal.today", "Go to today", "Ir a hoy", "calendar", Group::Calendar, "today hoy"),
    c("cal.prev", "Previous period", "Periodo anterior", "chevron_left", Group::Calendar, "back previous anterior"),
    c("cal.next", "Next period", "Periodo siguiente", "chevron_right", Group::Calendar, "forward next siguiente"),
    c("cal.view.day", "Calendar: day", "Calendario: día", "calendar", Group::Calendar, "day dia view vista"),
    c(
        "cal.view.work_week",
        "Calendar: work week",
        "Calendario: semana laboral",
        "calendar",
        Group::Calendar,
        "work week semana laboral",
    ),
    c("cal.view.week", "Calendar: week", "Calendario: semana", "calendar", Group::Calendar, "week semana"),
    c("cal.view.month", "Calendar: month", "Calendario: mes", "calendar", Group::Calendar, "month mes"),
    c(
        "cal.view.agenda",
        "Calendar: agenda",
        "Calendario: agenda",
        "list",
        Group::Calendar,
        "agenda list lista schedule",
    ),
    c("nav.today", "Go to My day", "Ir a Mi día", "sun", Group::Navigate, "today hoy home inicio resumen summary"),
    c("nav.mail", "Go to Mail", "Ir a Correo", "mail", Group::Navigate, "inbox bandeja"),
    c("nav.calendar", "Go to Calendar", "Ir a Calendario", "calendar", Group::Navigate, "agenda events citas"),
    c("nav.contacts", "Go to Contacts", "Ir a Contactos", "contacts", Group::Navigate, "people personas"),
    c("nav.notes", "Go to Notes", "Ir a Notas", "notes", Group::Navigate, "notes apuntes"),
    c("nav.tasks", "Go to Tasks", "Ir a Tareas", "tasks", Group::Navigate, "todo pendientes"),
    c("nav.assistant", "Go to Assistant", "Ir al Asistente", "sparkle", Group::Navigate, "ai ia assistant"),
    c("view.density.comfortable", "Density: comfortable", "Densidad: cómoda", "list", Group::View, "density"),
    c("view.density.compact", "Density: compact", "Densidad: compacta", "list", Group::View, "density"),
    c("view.density.dense", "Density: dense", "Densidad: densa", "list", Group::View, "density"),
    c(
        "view.theme.system",
        "Theme: system",
        "Tema: del sistema",
        "settings",
        Group::View,
        "theme appearance apariencia",
    ),
    c("view.theme.light", "Theme: light", "Tema: claro", "settings", Group::View, "theme light apariencia"),
    c("view.theme.dark", "Theme: dark", "Tema: oscuro", "settings", Group::View, "theme dark night noche"),
    c(
        "view.theme.high_contrast",
        "Theme: high contrast",
        "Tema: alto contraste",
        "settings",
        Group::View,
        "theme contrast accesibilidad",
    ),
    c("view.scheme.outlook", "Shortcuts: Outlook", "Atajos: Outlook", "settings", Group::View, "keyboard teclado"),
    c("view.scheme.gmail", "Shortcuts: Gmail", "Atajos: Gmail", "settings", Group::View, "keyboard teclado"),
    c(
        "view.scheme.apple_mail",
        "Shortcuts: Apple Mail",
        "Atajos: Apple Mail",
        "settings",
        Group::View,
        "keyboard teclado",
    ),
    c("app.palette", "Command palette", "Paleta de comandos", "search", Group::App, "commands comandos"),
    c("app.add_account", "Add account…", "Agregar cuenta…", "plus", Group::App, "account cuenta new nueva"),
    c("undo", "Undo", "Deshacer", "undo", Group::App, "undo revert deshacer"),
    // F4.2: asistente ✦, suite e importación (los atiende la UI: `Outcome::Assist`).
    c(
        "ai.summarize",
        "Summarize conversation",
        "Resumir conversación",
        "sparkle",
        Group::Assistant,
        "ai ia summary resumen",
    ),
    c(
        "ai.replies",
        "Suggest replies",
        "Sugerir respuestas",
        "sparkle",
        Group::Assistant,
        "ai ia reply respuesta rápida",
    ),
    c(
        "ai.analyze",
        "Analyze message",
        "Analizar mensaje",
        "sparkle",
        Group::Assistant,
        "ai ia dates fechas tareas lead ticket",
    ),
    c(
        "ai.compose",
        "Assistant in the composer",
        "Asistente en el redactor",
        "sparkle",
        Group::Assistant,
        "ai ia rewrite reescribir redactar tono",
    ),
    c(
        "ai.triage",
        "What needs my attention today?",
        "¿Qué necesito atender hoy?",
        "sparkle",
        Group::Assistant,
        "ai ia triage hoy prioridad",
    ),
    c(
        "ai.minutes",
        "Meeting minutes from a transcript",
        "Minuta desde una transcripción",
        "sparkle",
        Group::Assistant,
        "ai ia minutes minuta reunión transcribe",
    ),
    c(
        "ai.document",
        "Create document from selected mail",
        "Crear documento con los correos seleccionados",
        "sparkle",
        Group::Assistant,
        "ai ia document documento iureeditor",
    ),
    c(
        "ai.chat",
        "Chat with the assistant",
        "Conversar con el asistente",
        "sparkle",
        Group::Assistant,
        "ai ia chat preguntar",
    ),
    c(
        "app.import",
        "Import mail from Outlook, Thunderbird, Apple Mail…",
        "Importar correo de Outlook, Thunderbird, Apple Mail…",
        "plus",
        Group::App,
        "import importar migrar pst mbox",
    ),
    c(
        "suite.read_aloud",
        "Read message aloud (IureVoz)",
        "Leer el mensaje en voz alta (IureVoz)",
        "mail",
        Group::App,
        "voice voz leer speak",
    ),
    c(
        "suite.connect_agents",
        "Use IureDesk from Claude / Copilot",
        "Usar IureDesk desde Claude / Copilot",
        "settings",
        Group::App,
        "mcp claude copilot vscode agentes",
    ),
];

/// Busca un comando por id. Acepta ids con argumento (`mail.move:12` → `mail.move`).
pub fn find(id: &str) -> Option<&'static CommandSpec> {
    let base = id.split_once(':').map_or(id, |(b, _)| b);
    COMMANDS.iter().find(|c| c.id == base)
}

/// Atajo de un comando en un esquema.
pub fn shortcut(id: &str, scheme: ShortcutScheme) -> Option<Chord> {
    use ShortcutScheme::*;
    // Comunes a todos los esquemas.
    let common = match id {
        "app.palette" => Some(Chord::cmd("K")),
        "ai.compose" => Some(Chord::cmd("J")),
        "nav.mail" => Some(Chord::cmd("Num1")),
        "nav.calendar" => Some(Chord::cmd("Num2")),
        "nav.contacts" => Some(Chord::cmd("Num3")),
        "nav.tasks" => Some(Chord::cmd("Num4")),
        "nav.notes" => Some(Chord::cmd("Num5")),
        "nav.assistant" => Some(Chord::cmd("Num6")),
        _ => None,
    };
    if common.is_some() {
        return common;
    }
    // «Mi día»: Ctrl+0 lo usa egui para el zoom; «H» de «home».
    if id == "nav.today" {
        return Some(match scheme {
            Outlook | AppleMail => Chord::cmd_shift("H"),
            Gmail => Chord::shift("H"),
        });
    }
    // Calendario (solo se disparan en el módulo de calendario).
    let view = |n: &'static str| match scheme {
        Outlook => Chord { command: true, ctrl: false, shift: false, alt: true, key: n },
        Gmail | AppleMail => Chord::key(n),
    };
    let cal = match id {
        "cal.prev" => Some(Chord::key("ArrowLeft")),
        "cal.next" => Some(Chord::key("ArrowRight")),
        "cal.today" => Some(if scheme == AppleMail { Chord::cmd("T") } else { Chord::key("T") }),
        "cal.view.day" => Some(view("Num1")),
        "cal.view.work_week" => Some(view(if scheme == Outlook { "Num2" } else { "Num4" })),
        "cal.view.week" => Some(view(if scheme == Outlook { "Num3" } else { "Num2" })),
        "cal.view.month" => Some(view(if scheme == Outlook { "Num4" } else { "Num3" })),
        "cal.view.agenda" => Some(view("Num5")),
        _ => None,
    };
    if cal.is_some() {
        return cal;
    }
    match (scheme, id) {
        // Outlook para Windows.
        (Outlook, "mail.new") => Some(Chord::cmd("N")),
        (Outlook, "mail.reply") => Some(Chord::cmd("R")),
        (Outlook, "mail.reply_all") => Some(Chord::cmd_shift("R")),
        (Outlook, "mail.forward") => Some(Chord::cmd("F")),
        (Outlook, "mail.archive") => Some(Chord::key("Backspace")),
        (Outlook, "mail.delete") => Some(Chord::key("Delete")),
        (Outlook, "mail.toggle_read") => Some(Chord::cmd("Q")),
        (Outlook, "mail.toggle_flag") => Some(Chord::key("Insert")),
        (Outlook, "mail.move") => Some(Chord::cmd_shift("V")),
        (Outlook, "mail.search") => Some(Chord::cmd("E")),
        (Outlook, "undo") => Some(Chord::cmd("Z")),
        // Gmail (atajos de una tecla).
        (Gmail, "mail.new") => Some(Chord::key("C")),
        (Gmail, "mail.reply") => Some(Chord::key("R")),
        (Gmail, "mail.reply_all") => Some(Chord::key("A")),
        (Gmail, "mail.forward") => Some(Chord::key("F")),
        (Gmail, "mail.archive") => Some(Chord::key("E")),
        (Gmail, "mail.delete") => Some(Chord::shift("Num3")),
        (Gmail, "mail.toggle_read") => Some(Chord::shift("U")),
        (Gmail, "mail.toggle_flag") => Some(Chord::key("S")),
        (Gmail, "mail.move") => Some(Chord::key("V")),
        (Gmail, "mail.search") => Some(Chord::key("Slash")),
        (Gmail, "mail.snooze") => Some(Chord::key("B")),
        (Gmail, "undo") => Some(Chord::key("Z")),
        // Apple Mail.
        (AppleMail, "mail.new") => Some(Chord::cmd("N")),
        (AppleMail, "mail.reply") => Some(Chord::cmd("R")),
        (AppleMail, "mail.reply_all") => Some(Chord::cmd_shift("R")),
        (AppleMail, "mail.forward") => Some(Chord::cmd_shift("F")),
        (AppleMail, "mail.archive") => Some(Chord::cmd_ctrl("A")),
        (AppleMail, "mail.delete") => Some(Chord::key("Backspace")),
        (AppleMail, "mail.toggle_read") => Some(Chord::cmd_shift("U")),
        (AppleMail, "mail.toggle_flag") => Some(Chord::cmd_shift("L")),
        (AppleMail, "mail.search") => Some(Chord::cmd_alt("F")),
        (AppleMail, "undo") => Some(Chord::cmd("Z")),
        _ => None,
    }
}

/// Todos los atajos de un esquema (comando, combinación).
pub fn bindings(scheme: ShortcutScheme) -> Vec<(&'static str, Chord)> {
    COMMANDS.iter().filter_map(|c| shortcut(c.id, scheme).map(|k| (c.id, k))).collect()
}

/// El comando que dispara una combinación en un esquema.
pub fn command_for(chord: &Chord, scheme: ShortcutScheme) -> Option<&'static str> {
    COMMANDS.iter().find(|c| shortcut(c.id, scheme).as_ref() == Some(chord)).map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_have_both_labels() {
        let mut seen = std::collections::HashSet::new();
        for c in COMMANDS {
            assert!(seen.insert(c.id), "id repetido: {}", c.id);
            assert!(!c.en.is_empty() && !c.es.is_empty(), "{}", c.id);
        }
        for id in [
            "mail.new",
            "mail.reply",
            "mail.reply_all",
            "mail.forward",
            "mail.archive",
            "mail.delete",
            "mail.toggle_read",
            "mail.toggle_flag",
            "mail.move",
            "mail.search",
            "nav.today",
            "nav.mail",
            "nav.calendar",
            "nav.contacts",
            "nav.notes",
            "nav.tasks",
            "nav.assistant",
            "view.density.dense",
            "view.theme.dark",
            "app.palette",
            "app.add_account",
            "undo",
        ] {
            assert!(find(id).is_some(), "falta {id}");
        }
        assert_eq!(find("mail.move:12").map(|c| c.id), Some("mail.move"));
    }

    #[test]
    fn schemes_map_the_familiar_keys() {
        use ShortcutScheme::*;
        assert_eq!(shortcut("mail.reply", Outlook), Some(Chord::cmd("R")));
        assert_eq!(shortcut("mail.reply", Gmail), Some(Chord::key("R")));
        assert_eq!(shortcut("mail.archive", Gmail), Some(Chord::key("E")));
        assert_eq!(shortcut("mail.archive", Outlook), Some(Chord::key("Backspace")));
        assert_eq!(shortcut("mail.archive", AppleMail), Some(Chord::cmd_ctrl("A")));
        assert_eq!(shortcut("mail.toggle_flag", Outlook), Some(Chord::key("Insert")));
        assert_eq!(shortcut("mail.toggle_flag", Gmail), Some(Chord::key("S")));
        assert_eq!(shortcut("mail.forward", AppleMail), Some(Chord::cmd_shift("F")));
        // La paleta es Ctrl+K en todos.
        for s in ShortcutScheme::ALL {
            assert_eq!(shortcut("app.palette", s), Some(Chord::cmd("K")));
        }
        assert_eq!(command_for(&Chord::key("E"), Gmail), Some("mail.archive"));
        assert_eq!(command_for(&Chord::key("E"), Outlook), None);
        assert_eq!(command_for(&Chord::cmd("E"), Outlook), Some("mail.search"));
    }

    #[test]
    fn no_scheme_binds_one_chord_twice() {
        for s in ShortcutScheme::ALL {
            let b = bindings(s);
            for (i, (id, k)) in b.iter().enumerate() {
                for (id2, k2) in b.iter().skip(i + 1) {
                    assert!(k != k2, "{s:?}: {id} y {id2} comparten atajo");
                }
            }
        }
    }

    #[test]
    fn labels() {
        assert_eq!(Chord::cmd_shift("R").label(false), "Ctrl+Shift+R");
        assert_eq!(Chord::cmd_shift("R").label(true), "⇧⌘R");
        assert_eq!(Chord::shift("Num3").label(false), "#");
        assert_eq!(Chord::cmd("Num1").label(false), "Ctrl+1");
        assert_eq!(Chord::key("Slash").label(false), "/");
    }
}
