//! S1.12: lo que hacen las teclas del editor sobre el texto Markdown (funciones puras, sin egui).
//!
//! Las posiciones son **índices de carácter** (como los cursores de egui). Cada función devuelve
//! `None` si no le toca (la tecla hace lo normal) o el texto nuevo con la selección.
//!
//! - [`on_enter`]: en una lista, numerada, de tareas o en una cita, Intro sigue con el mismo
//!   marcador (el número siguiente, una casilla sin marcar); en un elemento vacío, sale de la
//!   lista. Dentro de un bloque de código, lo normal.
//! - [`indent`]: Tab / Mayús+Tab sangran o desangran un elemento de lista.
//! - [`toggle_wrap`]: Ctrl+B / Ctrl+I / tachado: pone o quita las marcas alrededor de la selección.
//! - [`link`]: Ctrl+K: `[texto](url)` con la `url` seleccionada para escribirla.

/// Resultado de una edición: el texto nuevo y la selección (índices de carácter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub sel_start: usize,
    pub sel_end: usize,
}

/// Índice de byte del carácter `ci` (o el final).
fn byte_of(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b)
}

fn char_of(text: &str, byte: usize) -> usize {
    text.get(..byte).map_or(0, |s| s.chars().count())
}

/// Bytes de la línea que contiene `byte`: (inicio, fin sin el salto).
fn line_at(text: &str, byte: usize) -> (usize, usize) {
    let start = text.get(..byte).and_then(|s| s.rfind('\n')).map_or(0, |i| i + 1);
    let end = text.get(byte..).and_then(|s| s.find('\n')).map_or(text.len(), |i| byte + i);
    (start, end)
}

/// ¿El byte `at` está dentro de un bloque de código cercado? (cercas antes de su línea).
fn in_fence(text: &str, at: usize) -> bool {
    let (start, _) = line_at(text, at);
    let before = text.get(..start).unwrap_or("");
    before
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("```") || t.starts_with("~~~")
        })
        .count()
        % 2
        == 1
}

/// Prefijo de bloque de una línea: sangría, marcador y casilla.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefix {
    /// Sangría (espacios).
    pub indent: String,
    /// `- `, `* `, `+ `, `3. `, `3) ` o `> `.
    pub marker: String,
    /// Número de una lista numerada.
    pub number: Option<u64>,
    /// `Some(marcada)` en una lista de tareas.
    pub task: Option<bool>,
    /// Bytes que ocupa todo el prefijo.
    pub len: usize,
}

/// El prefijo de lista o cita de `line`, si tiene.
pub fn prefix(line: &str) -> Option<Prefix> {
    let indent_len = line.len() - line.trim_start_matches(' ').len();
    let indent = line.get(..indent_len)?.to_string();
    let rest = line.get(indent_len..)?;
    let (marker, number) = if let Some(r) = rest.strip_prefix("> ") {
        let _ = r;
        ("> ".to_string(), None)
    } else if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        (rest.get(..2)?.to_string(), None)
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if !(1..=9).contains(&digits) {
            return None;
        }
        let delim = rest.get(digits..digits + 2)?;
        if delim != ". " && delim != ") " {
            return None;
        }
        (rest.get(..digits + 2)?.to_string(), rest.get(..digits)?.parse().ok())
    };
    let after = rest.get(marker.len()..).unwrap_or("");
    let task = if marker != "> " {
        if after.starts_with("[ ] ") || after == "[ ]" {
            Some(false)
        } else if after.starts_with("[x] ") || after.starts_with("[X] ") || after == "[x]" || after == "[X]" {
            Some(true)
        } else {
            None
        }
    } else {
        None
    };
    let task_len = if task.is_some() { after.len().min(4) } else { 0 };
    Some(Prefix { indent, len: indent_len + marker.len() + task_len, marker, number, task })
}

/// Intro en una lista o cita: sigue con el marcador, o sale si el elemento está vacío.
pub fn on_enter(text: &str, cursor: usize) -> Option<Edit> {
    let at = byte_of(text, cursor);
    if in_fence(text, at) {
        return None;
    }
    let (start, end) = line_at(text, at);
    let line = text.get(start..end)?;
    let p = prefix(line)?;
    // El cursor dentro del prefijo: lo normal.
    if at < start + p.len {
        return None;
    }
    let content = line.get(p.len..).unwrap_or("").trim();
    if content.is_empty() {
        // Elemento vacío: se quita el marcador (sale de la lista o de la cita).
        let mut out = String::with_capacity(text.len());
        out.push_str(text.get(..start)?);
        out.push_str(text.get(end..)?);
        let c = char_of(&out, start);
        return Some(Edit { text: out, sel_start: c, sel_end: c });
    }
    let marker = match p.number {
        Some(n) => {
            let delim = p.marker.chars().rev().nth(1).unwrap_or('.');
            format!("{}{delim} ", n.saturating_add(1))
        }
        None => p.marker.clone(),
    };
    let task = if p.task.is_some() { "[ ] " } else { "" };
    let insert = format!("\n{}{marker}{task}", p.indent);
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(text.get(..at)?);
    out.push_str(&insert);
    out.push_str(text.get(at..)?);
    let c = cursor + insert.chars().count();
    Some(Edit { text: out, sel_start: c, sel_end: c })
}

/// Tab (o Mayús+Tab con `outdent`) en un elemento de lista: dos espacios más o menos al principio.
pub fn indent(text: &str, cursor: usize, outdent: bool) -> Option<Edit> {
    let at = byte_of(text, cursor);
    if in_fence(text, at) {
        return None;
    }
    let (start, end) = line_at(text, at);
    let line = text.get(start..end)?;
    let p = prefix(line)?;
    if p.marker == "> " {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push_str(text.get(..start)?);
    let delta: isize = if outdent {
        let remove = p.indent.len().min(2);
        if remove == 0 {
            return None;
        }
        out.push_str(line.get(remove..)?);
        -(remove as isize)
    } else {
        out.push_str("  ");
        out.push_str(line);
        2
    };
    out.push_str(text.get(end..)?);
    let c = cursor.saturating_add_signed(delta).max(char_of(text, start));
    Some(Edit { text: out, sel_start: c, sel_end: c })
}

/// Pone (o quita, si ya están) las marcas `mark` alrededor de la selección. Sin selección, deja
/// el cursor entre las marcas.
pub fn toggle_wrap(text: &str, sel_start: usize, sel_end: usize, mark: &str) -> Edit {
    let (a, b) = (sel_start.min(sel_end), sel_start.max(sel_end));
    let (ab, bb) = (byte_of(text, a), byte_of(text, b));
    let m = mark.chars().count();
    let before = text.get(..ab).unwrap_or("");
    let after = text.get(bb..).unwrap_or("");
    let inside = text.get(ab..bb).unwrap_or("");
    // Ya rodeada: se quitan las marcas.
    if before.ends_with(mark) && after.starts_with(mark) && !inside.is_empty() {
        let out = format!(
            "{}{inside}{}",
            before.get(..before.len() - mark.len()).unwrap_or(""),
            after.get(mark.len()..).unwrap_or("")
        );
        return Edit { text: out, sel_start: a - m, sel_end: b - m };
    }
    if inside.starts_with(mark) && inside.ends_with(mark) && inside.chars().count() >= 2 * m {
        let core = inside.get(mark.len()..inside.len() - mark.len()).unwrap_or("");
        let out = format!("{before}{core}{after}");
        return Edit { text: out, sel_start: a, sel_end: b - 2 * m };
    }
    let out = format!("{before}{mark}{inside}{mark}{after}");
    Edit { text: out, sel_start: a + m, sel_end: b + m }
}

/// Ctrl+K: `[selección](url)` con «url» seleccionada para escribir la dirección.
pub fn link(text: &str, sel_start: usize, sel_end: usize) -> Edit {
    let (a, b) = (sel_start.min(sel_end), sel_start.max(sel_end));
    let (ab, bb) = (byte_of(text, a), byte_of(text, b));
    let inside = text.get(ab..bb).unwrap_or("");
    let label = if inside.is_empty() { "texto" } else { inside };
    let out = format!("{}[{label}](url){}", text.get(..ab).unwrap_or(""), text.get(bb..).unwrap_or(""));
    let url_at = a + 1 + label.chars().count() + 2;
    Edit { text: out, sel_start: url_at, sel_end: url_at + 3 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, marker: char) -> (String, usize) {
        let i = text.chars().position(|c| c == marker).unwrap();
        (text.replacen(marker, "", 1), i)
    }

    #[test]
    fn enter_continues_lists_and_quotes() {
        let (t, c) = at("- uno|", '|');
        assert_eq!(on_enter(&t, c).unwrap().text, "- uno\n- ");
        let (t, c) = at("  3. tres|\n", '|');
        assert_eq!(on_enter(&t, c).unwrap().text, "  3. tres\n  4. \n");
        let (t, c) = at("1) a|", '|');
        assert_eq!(on_enter(&t, c).unwrap().text, "1) a\n2) ");
        let (t, c) = at("- [x] hecha|", '|');
        let e = on_enter(&t, c).unwrap();
        assert_eq!(e.text, "- [x] hecha\n- [ ] ");
        assert_eq!(e.sel_start, e.text.chars().count());
        let (t, c) = at("> cita|", '|');
        assert_eq!(on_enter(&t, c).unwrap().text, "> cita\n> ");
        // Partir un elemento por la mitad.
        let (t, c) = at("- uno| dos", '|');
        assert_eq!(on_enter(&t, c).unwrap().text, "- uno\n-  dos");
    }

    #[test]
    fn enter_on_empty_item_leaves_the_list() {
        let (t, c) = at("- uno\n- |", '|');
        let e = on_enter(&t, c).unwrap();
        assert_eq!(e.text, "- uno\n");
        assert_eq!(e.sel_start, 6);
        let (t, c) = at("- [ ] |", '|');
        assert_eq!(on_enter(&t, c).unwrap().text, "");
    }

    #[test]
    fn enter_is_normal_elsewhere() {
        for s in ["párrafo|", "```\n- en código|\n```", "-sin espacio|", "|- al principio"] {
            let (t, c) = at(s, '|');
            assert!(on_enter(&t, c).is_none(), "{s}");
        }
    }

    #[test]
    fn tab_indents_list_items() {
        let (t, c) = at("- uno\n- dos|", '|');
        let e = indent(&t, c, false).unwrap();
        assert_eq!(e.text, "- uno\n  - dos");
        assert_eq!(e.sel_start, c + 2);
        let back = indent(&e.text, e.sel_start, true).unwrap();
        assert_eq!(back.text, t);
        assert!(indent(&t, c, true).is_none(), "sin sangría que quitar");
        assert!(indent("texto", 2, false).is_none());
    }

    #[test]
    fn wrap_and_unwrap() {
        let e = toggle_wrap("hola mundo", 5, 10, "**");
        assert_eq!(e.text, "hola **mundo**");
        assert_eq!((e.sel_start, e.sel_end), (7, 12));
        let back = toggle_wrap(&e.text, e.sel_start, e.sel_end, "**");
        assert_eq!(back.text, "hola mundo");
        let inner = toggle_wrap("a *b* c", 2, 5, "*");
        assert_eq!(inner.text, "a b c");
        let empty = toggle_wrap("ab", 1, 1, "*");
        assert_eq!((empty.text.as_str(), empty.sel_start), ("a**b", 2));
        let ñ = toggle_wrap("año ñu", 4, 6, "~~");
        assert_eq!(ñ.text, "año ~~ñu~~");
    }

    #[test]
    fn link_selects_the_url() {
        let e = link("ver acá", 4, 7);
        assert_eq!(e.text, "ver [acá](url)");
        let sel: String = e.text.chars().skip(e.sel_start).take(e.sel_end - e.sel_start).collect();
        assert_eq!(sel, "url");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(text in "(\\PC|- |> |1\\. |\\[ \\] |```|\n|  ){0,60}", c in 0usize..80, d in 0usize..80) {
            let n = text.chars().count();
            let (c, d) = (c.min(n), d.min(n));
            let _ = on_enter(&text, c);
            let _ = indent(&text, c, false);
            let _ = indent(&text, c, true);
            let e = toggle_wrap(&text, c, d, "**");
            proptest::prop_assert!(e.sel_end <= e.text.chars().count());
            let l = link(&text, c, d);
            proptest::prop_assert!(l.sel_end <= l.text.chars().count());
        }
    }
}
