//! Descripción de un evento para el editor (dueño 2026-11-03: una cita de Google se veía como
//! HTML crudo).
//!
//! Google Calendar guarda la descripción como HTML (`<p>`, `<strong>`, `<ol>`, `<br>`…) y le pega
//! al final, en texto, el bloque de la videollamada entre dos líneas `-::~:~::~…::-` («Únase con
//! Google Meet…», teléfonos, «No edites esta sección»). [`split`] separa ese bloque y, si lo
//! demás es HTML, lo pasa a Markdown (lo que el editor muestra y la vista previa pinta).
//! [`join`] hace lo contrario al guardar: si la descripción no cambió se devuelve la original
//! tal cual; si cambió y era HTML, se vuelve a escribir como HTML (para que Google y Gmail la
//! sigan viendo con formato), y el bloque de la videollamada se conserva siempre.

use iuredesk_compose::RichBody;

/// Largo máximo que se convierte (lo demás se deja como texto).
const MAX_HTML: usize = 256 * 1024;

/// Descripción preparada para editar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditDescription {
    /// Lo que se muestra y edita (Markdown si venía en HTML).
    pub text: String,
    /// Venía en HTML (al guardar un cambio se vuelve a escribir en HTML).
    pub html: bool,
    /// Bloque de la videollamada que añade Google (con sus líneas de marca), tal cual.
    pub conference: String,
}

/// ¿Es la línea de marca del bloque de Google (`-::~:~::~…::-`)?
fn is_marker(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 10 && t.starts_with("-::~") && t.ends_with("::-") && t.chars().all(|c| matches!(c, '-' | ':' | '~'))
}

/// Separa el bloque de la videollamada de Google: (antes, bloque). El bloque va de la primera
/// línea de marca hasta la última (o el final).
fn split_conference(s: &str) -> (&str, &str) {
    let mut start = None;
    let mut pos = 0usize;
    for line in s.split_inclusive('\n') {
        if is_marker(line) {
            start = Some(pos);
            break;
        }
        pos += line.len();
    }
    match start {
        Some(i) => (s.get(..i).unwrap_or(s), s.get(i..).unwrap_or("")),
        None => (s, ""),
    }
}

/// Etiquetas que delatan HTML (en minúsculas, sin `<`).
const TAGS: [&str; 22] = [
    "p",
    "br",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "ol",
    "ul",
    "li",
    "a",
    "div",
    "span",
    "h1",
    "h2",
    "h3",
    "h4",
    "table",
    "tr",
    "td",
    "font",
    "blockquote",
];

/// ¿Parece HTML? Hace falta una etiqueta conocida **cerrada** (`</p>`, `</strong>`…) o un
/// salto `<br>`: un «a <b c» o «1 < 2» escrito a mano no lo es.
pub fn looks_like_html(s: &str) -> bool {
    let lower = s.get(..s.len().min(MAX_HTML)).unwrap_or(s).to_ascii_lowercase();
    let closed = TAGS.iter().any(|t| lower.contains(&format!("</{t}>")));
    closed || ["<br>", "<br/>", "<br />"].iter().any(|b| lower.contains(b))
}

/// HTML → Markdown. Los saltos de línea fuera de etiquetas cuentan (Google mezcla HTML y texto).
fn html_to_markdown(html: &str) -> String {
    let mut prepared = String::with_capacity(html.len() + 64);
    for line in html.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        prepared.push_str(body);
        if line.ends_with('\n') {
            // Un salto tras texto es un salto de verdad; tras una etiqueta, solo formato.
            if body.trim_end().ends_with('>') || body.trim().is_empty() {
                prepared.push('\n');
            } else {
                prepared.push_str("<br>\n");
            }
        }
    }
    // Saltos duros como dos espacios (lo mismo en Markdown, sin la `\` a la vista).
    RichBody::from_html(&prepared).to_markdown().replace("\\\n", "  \n").trim().to_string()
}

/// Descripción del evento → lo que muestra el editor.
pub fn split(raw: &str) -> EditDescription {
    let (main, conference) = split_conference(raw);
    let html = main.len() <= MAX_HTML && looks_like_html(main);
    let text = if html { html_to_markdown(main) } else { main.trim_end().to_string() };
    EditDescription { text, html, conference: conference.to_string() }
}

/// Lo editado → la descripción a guardar. `original` es la del evento (si lo había): si el
/// texto no cambió, se devuelve igual, byte por byte.
pub fn join(edited: &str, shown: &EditDescription, original: Option<&str>) -> String {
    if let Some(o) = original
        && edited == shown.text
    {
        return o.to_string();
    }
    let mut out = if shown.html && !edited.trim().is_empty() {
        RichBody::from_markdown(edited).to_html_fragment()
    } else {
        edited.to_string()
    };
    if !shown.conference.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&shown.conference);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE: &str = "<p><strong>Agenda de la reunión</strong><br>📅 <strong>Fecha:</strong> 3 de noviembre de 2026<br>🕐 <strong>Hora:</strong> 13:00 hrs</p><p><strong>Temas a tratar:</strong></p><ol><li><p><strong>Fintechs:</strong> Planear que Jorge vea a dos personas</p></li><li><p><strong>Estatus:</strong> Dar un estatus de AI</p></li></ol>\n\n-::~:~::~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~::~:~::-\nÚnase con Google Meet: https://meet.google.com/dhi-jbhv-smg\n\nO marca: (MX) +52 55 8421 0898 PIN: 1623699540517#\n\nNo edites esta sección.\n-::~:~::~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~::~:~::-";

    #[test]
    fn google_html_becomes_markdown_and_the_meet_block_is_set_apart() {
        // Regresión (dueño 2026-11-03): la descripción se veía como HTML crudo.
        let d = split(GOOGLE);
        assert!(d.html);
        assert!(!d.text.contains("<strong>") && !d.text.contains("<p>"), "{}", d.text);
        assert!(d.text.contains("**Agenda de la reunión**"), "{}", d.text);
        assert!(d.text.contains("**Fecha:** 3 de noviembre de 2026"), "{}", d.text);
        assert!(!d.text.contains("\\\n"), "sin barras de salto a la vista: {}", d.text);
        assert!(d.text.contains("1. ") && d.text.contains("**Fintechs:**"), "{}", d.text);
        assert!(!d.text.contains("Únase"), "el bloque de Meet va aparte");
        assert!(d.conference.starts_with("-::~") && d.conference.contains("meet.google.com/dhi-jbhv-smg"));
    }

    #[test]
    fn unchanged_description_is_kept_byte_for_byte() {
        let d = split(GOOGLE);
        assert_eq!(join(&d.text, &d, Some(GOOGLE)), GOOGLE);
    }

    #[test]
    fn edited_html_is_written_back_as_html_with_the_meet_block() {
        let d = split(GOOGLE);
        let edited = format!("{}\n\nAgregado **aquí**", d.text);
        let out = join(&edited, &d, Some(GOOGLE));
        assert!(out.contains("<strong>aquí</strong>"), "{out}");
        assert!(out.contains("<strong>Agenda de la reunión</strong>"), "{out}");
        assert!(out.trim_end().ends_with("::-") && out.contains("No edites esta sección."), "{out}");
    }

    #[test]
    fn plain_text_stays_plain() {
        for s in ["Llamar a Ana < 5 min", "a <b c", "Sala 2\nPiso 3", "x<brillante>"] {
            let d = split(s);
            assert!(!d.html, "{s}");
            assert_eq!(join(&d.text, &d, Some(s)), s);
        }
        let d = split("Hola");
        assert_eq!(join("Hola de nuevo", &d, Some("Hola")), "Hola de nuevo");
        assert_eq!(join("Nuevo", &EditDescription::default(), None), "Nuevo");
    }

    #[test]
    fn html_detection() {
        assert!(looks_like_html("<p>x</p>"));
        assert!(looks_like_html("a<br>b"));
        assert!(looks_like_html("<A HREF=\"x\">y</A>"));
        assert!(!looks_like_html("1 < 2 y 3 > 2"));
        assert!(!looks_like_html("<presupuesto>"));
    }

    proptest::proptest! {
        #[test]
        fn never_panics_and_unchanged_roundtrips(s in "(\\PC|<p>|</p>|<br>|<strong>|\n|-::~:~::~::-){0,120}") {
            let d = split(&s);
            proptest::prop_assert_eq!(join(&d.text, &d, Some(&s)), s);
        }
    }
}
