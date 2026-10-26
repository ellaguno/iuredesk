//! Hoja de agente de usuario «de correo».
//!
//! Blitz traduce a CSS algunos atributos de presentación (`width`, `height`, `bgcolor`, `align` como
//! `text-align`, `hspace`…), pero no los que más usan los correos antiguos: `border`, `cellpadding`,
//! `cellspacing`, `valign`, `<table align=center>`, `<img align=left>`, `<font size>`, `<br clear>`.
//! Aquí se emulan con selectores de atributo en una hoja de origen «agente de usuario», de modo que
//! cualquier regla del propio correo los vence (como en un navegador, donde estos atributos tienen
//! especificidad cero). Los valores libres (`<font face/color>`, `background=`) los resuelve el
//! saneador como estilo en línea.

use std::fmt::Write as _;

pub(crate) fn email_ua_css() -> String {
    let mut css = String::with_capacity(16 * 1024);
    // Como Gmail/Apple Mail: las URL largas no ensanchan el mensaje.
    css.push_str("html { overflow-wrap: break-word; }\n");

    // border="N" en tablas: marco de N px y celdas de 1 px.
    css.push_str(
        "table[border]:not([border=\"0\"]) { border: 1px outset #808080; }\n\
         table[border]:not([border=\"0\"]) > * > tr > td, table[border]:not([border=\"0\"]) > * > tr > th \
         { border: 1px inset #808080; }\n",
    );
    for n in 2..=10 {
        let _ = writeln!(css, "table[border=\"{n}\"] {{ border-width: {n}px; }}");
    }
    // bordercolor (no estándar, pero frecuente): solo nombres simples vía atributo exacto no sirve;
    // se deja al saneador.

    // cellpadding / cellspacing.
    for n in 0..=40 {
        let _ = writeln!(
            css,
            "table[cellpadding=\"{n}\"] > * > tr > td, table[cellpadding=\"{n}\"] > * > tr > th {{ padding: {n}px; }}"
        );
    }
    for n in 0..=30 {
        let _ = writeln!(css, "table[cellspacing=\"{n}\"] {{ border-spacing: {n}px; }}");
    }

    // valign en filas y celdas.
    for v in ["top", "middle", "bottom", "baseline"] {
        let _ = writeln!(
            css,
            "td[valign=\"{v}\" i], th[valign=\"{v}\" i], tr[valign=\"{v}\" i] > td, tr[valign=\"{v}\" i] > th, \
             tbody[valign=\"{v}\" i] > tr > td {{ vertical-align: {v}; }}"
        );
    }

    // Alineación de tablas e imágenes.
    css.push_str(
        "table[align=\"center\" i] { margin-left: auto; margin-right: auto; }\n\
         img[align=\"left\" i] { float: left; }\n\
         img[align=\"right\" i] { float: right; }\n\
         img[align=\"middle\" i], img[align=\"absmiddle\" i], img[align=\"center\" i] { vertical-align: middle; }\n\
         img[align=\"top\" i], img[align=\"texttop\" i] { vertical-align: top; }\n\
         img[align=\"bottom\" i], img[align=\"absbottom\" i], img[align=\"baseline\" i] { vertical-align: baseline; }\n\
         br[clear=\"all\" i], br[clear=\"both\" i] { clear: both; }\n\
         br[clear=\"left\" i] { clear: left; }\n\
         br[clear=\"right\" i] { clear: right; }\n",
    );

    // <font size>: 1–7 y relativos a 3.
    let sizes = ["x-small", "small", "medium", "large", "x-large", "xx-large", "48px"];
    for (i, s) in sizes.iter().enumerate() {
        let n = i + 1;
        let _ = writeln!(css, "font[size=\"{n}\"] {{ font-size: {s}; }}");
        let rel = n as i32 - 3;
        if rel != 0 {
            let _ = writeln!(css, "font[size=\"{rel:+}\"] {{ font-size: {s}; }}");
        }
    }

    // <ol type>.
    css.push_str(
        "ol[type=\"1\"] { list-style-type: decimal; }\n\
         ol[type=\"a\" s] { list-style-type: lower-alpha; }\n\
         ol[type=\"A\" s] { list-style-type: upper-alpha; }\n\
         ol[type=\"i\" s] { list-style-type: lower-roman; }\n\
         ol[type=\"I\" s] { list-style-type: upper-roman; }\n\
         hr[noshade] { border-style: solid; background-color: gray; }\n",
    );
    css
}
