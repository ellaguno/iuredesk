//! UX-12: diagramas Mermaid de las notas → SVG con `mermaid-svg` (Rust puro, solo depende de
//! `thiserror`; aprobado por el dueño 2026-11-03).
//!
//! El texto de una nota es hostil: el crate no promete no entrar en pánico ni acotar su
//! trabajo, así que aquí se le pone el cerco:
//! - código acotado ([`MAX_SOURCE`] bytes, [`MAX_LINES`] líneas);
//! - corre en su propio hilo, dentro de `catch_unwind`, y se espera como mucho [`TIMEOUT`] (si no
//!   termina, se deja y la nota muestra el código);
//! - el SVG que sale se acota en tamaño y pasa por [`crate::net::svg_is_safe`] (nada de
//!   referencias externas ni scripts) antes de entregarse como imagen.
//!
//! El tema (claro u oscuro) y el tamaño de letra salen de los colores de la nota.

use std::sync::mpsc;
use std::time::Duration;

use crate::markdown::NoteStyle;

/// Bytes de código como mucho.
pub const MAX_SOURCE: usize = 16 * 1024;
/// Líneas de código como mucho.
pub const MAX_LINES: usize = 600;
/// Tiempo máximo de dibujo.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// SVG resultante como mucho.
const MAX_SVG: usize = 4 * 1024 * 1024;

/// ¿Fondo oscuro? (texto claro).
fn dark(style: &NoteStyle) -> bool {
    let [r, g, b] = style.text;
    u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 128_000
}

/// Tema de Mermaid con el claro u oscuro y la letra de la nota.
pub(crate) fn theme_for(style: &NoteStyle) -> Option<mermaid_svg::Theme> {
    let mut theme =
        if dark(style) { mermaid_svg::Theme::by_name("dark")? } else { mermaid_svg::Theme::by_name("default")? };
    theme.font_size = f64::from(style.font_px.clamp(10.0, 24.0));
    // Tamaño fijo en píxeles (sin `width="100%"`): Blitz lo pinta a su tamaño.
    theme.responsive = false;
    Some(theme)
}

/// Dibuja el diagrama. `None` si no se puede (sintaxis, tamaño, tiempo, pánico, SVG raro).
pub fn to_svg(code: &str, style: &NoteStyle) -> Option<String> {
    render(code, theme_for(style)?)
}

/// S1.19: el mapa mental en distribución radial, con los colores de rama del dibujo propio
/// (contraste AA con texto blanco; los pasteles de Mermaid casi no se ven sobre fondo claro) y el
/// centro en el color de acento.
pub fn mindmap_svg(root: &crate::mindmap::Node, shape: crate::mindmap::Shape, style: &NoteStyle) -> Option<String> {
    let mut theme = theme_for(style)?;
    branch_palette(&mut theme, style);
    render(&mindmap_code(root, shape), theme)
}

/// Colores de rama propios en un tema de Mermaid (mapas mentales) y el centro en el acento.
pub(crate) fn branch_palette(theme: &mut mermaid_svg::Theme, style: &NoteStyle) {
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    // `mermaid-svg` pinta la rama `k` con el color `k + 1` de la escala: se corre un lugar para que
    // la primera rama tenga el mismo color que en las demás distribuciones.
    let n = crate::mindmap::BRANCH.len();
    theme.cscale_palette = (0..n)
        .filter_map(|k| crate::mindmap::BRANCH.get((k + n - 1) % n))
        .map(|c| hex(*c).into())
        .collect::<Vec<_>>()
        .into();
    theme.git_palette = vec![hex(style.accent).into()].into();
}

/// Dibuja con `mermaid-svg`, acotado: código, hilo propio con `catch_unwind`, tiempo y SVG.
pub(crate) fn render(code: &str, theme: mermaid_svg::Theme) -> Option<String> {
    let code = code.trim();
    if code.is_empty() || code.len() > MAX_SOURCE || code.lines().count() > MAX_LINES {
        return None;
    }
    let code = code.to_string();
    let (tx, rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new().name("mermaid".into()).stack_size(8 * 1024 * 1024).spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mermaid_svg::render_with(&code, &theme)));
        let _ = tx.send(r.ok().and_then(Result::ok));
    });
    if spawned.is_err() {
        return None;
    }
    let svg = rx.recv_timeout(TIMEOUT).ok().flatten()?;
    (svg.len() <= MAX_SVG && svg.trim_start().starts_with("<svg") && crate::net::svg_is_safe(svg.as_bytes()))
        .then_some(svg)
}

/// S1.19: un mapa mental de IureEditor como diagrama `mindmap` de Mermaid (distribución radial).
/// Etiquetas ya planas ([`crate::mindmap::plain_label`]); aquí se cambian los caracteres que
/// Mermaid lee como forma o comentario por sus parecidos (como hace IureEditor).
pub fn mindmap_code(root: &crate::mindmap::Node, shape: crate::mindmap::Shape) -> String {
    let mut out = String::from("mindmap\n");
    let mut next = 0usize;
    mindmap_lines(root, shape, 1, &mut next, &mut out);
    out
}

fn mindmap_lines(
    n: &crate::mindmap::Node,
    shape: crate::mindmap::Shape,
    depth: usize,
    next: &mut usize,
    out: &mut String,
) {
    use std::fmt::Write as _;
    let label = safe_label(&n.label);
    let indent = "  ".repeat(depth);
    // Sin forma, la línea es solo el texto (un id delante se leería como parte de él).
    let _ = match shape.delims() {
        Some((open, close)) => writeln!(out, "{indent}n{}{open}{label}{close}", *next),
        None => writeln!(out, "{indent}{label}"),
    };
    *next += 1;
    for c in &n.children {
        mindmap_lines(c, shape, depth + 1, next, out);
    }
}

fn safe_label(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| match c {
            '(' => '（',
            ')' => '）',
            '[' => '［',
            ']' => '］',
            '{' => '｛',
            '}' => '｝',
            '<' => '＜',
            '>' => '＞',
            '*' => '∗',
            '`' => 'ʼ',
            '"' => '＂',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    // `%%` abre un comentario y `::` una clase o un icono.
    out = out.replace("%%", "%％").replace("::", ":：");
    if out.trim().is_empty() { "·".to_string() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GANTT: &str = "gantt\n    title Plan\n    dateFormat YYYY-MM-DD\n    section Fase 1\n    Tarea A :a1, 2024-01-01, 7d\n    Tarea B :after a1, 5d\n";

    #[test]
    fn draws_the_owner_gantt() {
        // La nota del dueño (2026-11-03): el Gantt salía como código.
        let s = to_svg(GANTT, &NoteStyle::default()).unwrap();
        assert!(s.starts_with("<svg") && s.contains("Tarea A") && s.contains("Plan"));
        assert!(!s.contains("width=\"100%\""), "tamaño fijo");
    }

    #[test]
    fn flowchart_sequence_and_dark_theme() {
        let flow = "flowchart LR\n  A[Inicio] --> B{¿Listo?}\n  B -- Sí --> C[Enviar]\n";
        let light = to_svg(flow, &NoteStyle::default()).unwrap();
        let dark_style = NoteStyle { text: [0xe6, 0xe8, 0xeb], ..NoteStyle::default() };
        let dark = to_svg(flow, &dark_style).unwrap();
        assert!(light.contains("¿Listo?") && dark.contains("¿Listo?"));
        assert_ne!(light, dark, "el tema sigue a la nota");
        assert!(to_svg("sequenceDiagram\n  Ana->>Beto: Hola\n", &NoteStyle::default()).is_some());
    }

    #[test]
    fn bad_or_huge_code_is_not_drawn() {
        let st = NoteStyle::default();
        assert_eq!(to_svg("flowchart TD\nA-->", &st), None);
        assert_eq!(to_svg("esto no es mermaid", &st), None);
        assert_eq!(to_svg("", &st), None);
        let big = format!("flowchart TD\n{}", "A-->B\n".repeat(MAX_LINES + 1));
        assert_eq!(to_svg(&big, &st), None);
        let long = format!("flowchart TD\nA[{}]-->B", "x".repeat(MAX_SOURCE));
        assert_eq!(to_svg(&long, &st), None);
    }

    #[test]
    fn labels_are_escaped() {
        let s = to_svg("flowchart LR\n  A[\"<script>alert(1)</script>\"] --> B", &NoteStyle::default());
        if let Some(s) = s {
            assert!(!s.contains("<script"), "{s}");
        }
    }

    #[test]
    fn mindmap_code_is_safe_mermaid() {
        use crate::mindmap::{Node, Shape};
        let root = Node {
            label: "Centro (v2) %% x".into(),
            children: vec![
                Node {
                    label: "a::b [c] {d}".into(),
                    children: vec![Node { label: "<script>".into(), children: vec![] }],
                },
                Node { label: String::new(), children: vec![] },
            ],
        };
        let code = mindmap_code(&root, Shape::Cloud);
        assert!(code.starts_with("mindmap\n  n0)Centro （v2） %％ x(\n"), "{code}");
        assert!(code.contains("    n1)a:：b ［c］ ｛d｝(\n") && code.contains("      n2)＜script＞(\n"), "{code}");
        assert!(code.contains("    n3)·(\n"), "{code}");
        let svg = to_svg(&code, &NoteStyle::default()).unwrap();
        assert!(svg.contains("Centro") && !svg.contains("<script"));
        // Con los colores de rama propios (no los pasteles de Mermaid) y el centro en el acento.
        let ours = mindmap_svg(&root, Shape::Cloud, &NoteStyle::default()).unwrap();
        // La primera rama, azul como en las demás distribuciones; la segunda, pizarra.
        assert!(ours.contains("fill=\"#1d4ed8\"") && ours.contains("fill=\"#475569\""), "{ours}");
        assert!(ours.contains("fill=\"#1a5fb4\""), "centro en el acento");
        assert!(!ours.contains("#FFFFAB") && svg.contains("#FFFFAB"));
        for shape in Shape::ALL {
            assert!(to_svg(&mindmap_code(&root, shape), &NoteStyle::default()).is_some(), "{shape:?}");
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]
        #[test]
        fn never_panics(body in "(\\PC|-->|---|\\[|\\]|\\{|\\}|\\(|\\)|:|;|\n|%%){0,200}",
                        kind in proptest::sample::select(vec!["flowchart TD", "graph LR", "sequenceDiagram", "gantt", "pie", "classDiagram", "stateDiagram-v2", "erDiagram", "mindmap", "xychart-beta", "kanban", "treemap", "timeline", "journey", "gitGraph"])) {
            let _ = to_svg(&format!("{kind}\n{body}"), &NoteStyle::default());
        }
    }
}
