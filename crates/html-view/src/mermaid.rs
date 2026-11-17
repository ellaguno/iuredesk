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
pub(crate) const MAX_SVG: usize = 4 * 1024 * 1024;

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
    render_raw(code, theme).filter(|svg| crate::net::svg_is_safe(svg.as_bytes()))
}

/// Como [`render`] sin el filtro de referencias externas: quien llama quita los enlaces de los
/// nodos ([`crate::diagram`]) y filtra después.
pub(crate) fn render_raw(code: &str, theme: mermaid_svg::Theme) -> Option<String> {
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
    (svg.len() <= MAX_SVG && svg.trim_start().starts_with("<svg")).then_some(svg)
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

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]
        #[test]
        fn never_panics(body in "(\\PC|-->|---|\\[|\\]|\\{|\\}|\\(|\\)|:|;|\n|%%){0,200}",
                        kind in proptest::sample::select(vec!["flowchart TD", "graph LR", "sequenceDiagram", "gantt", "pie", "classDiagram", "stateDiagram-v2", "erDiagram", "mindmap", "xychart-beta", "kanban", "treemap", "timeline", "journey", "gitGraph"])) {
            let _ = to_svg(&format!("{kind}\n{body}"), &NoteStyle::default());
        }
    }
}
