//! S1.11: el documento de la suite Iurefficient (capa L1, sin E/S ni egui).
//!
//! Notas, minutas, documentos de casos y transcripciones son **Markdown**; este crate los lee como
//! un modelo de bloques que un editor puede cambiar, y los vuelve a escribir **sin pérdidas**:
//!
//! - Cada bloque de primer nivel guarda su texto original (con el espacio que lo sigue), y los
//!   trozos cubren el archivo entero. Guardar sin tocar nada da el mismo archivo byte a byte; un
//!   bloque que no se tocó se escribe igual aunque se hayan cambiado otros. Así los diffs son
//!   limpios, IureEditor y otros editores conviven y lo que este modelo no entiende sobrevive.
//! - Solo los bloques cambiados ([`Document::set`], [`Document::insert`]) se escriben desde el
//!   modelo ([`write`]), con un estilo fijo (`-` para viñetas, `**`, `*`, `~~`…).
//! - Lo que no se modela (HTML, notas al pie, definiciones…) es [`Block::Raw`]: se conserva tal
//!   cual.
//! - **Metadatos por bloque** ([`Meta`]): un comentario HTML justo antes del bloque,
//!   `<!-- iure:segment {"speaker":"Ana","start":"00:01:02"} -->`. Otros editores no lo muestran y
//!   no se pierde; sirve para segmentos de transcripción, referencias a tareas, etc. (bloques
//!   extensibles que registra la app anfitriona).
//! - Avisos y mapas mentales de IureEditor (`> [!NOTE] …`, `> [!MINDMAP|map] Centro`) son
//!   [`Block::Callout`] con su tipo y su metadato.
//!
//! Todo lo que llega es hostil: anidado, número de bloques y tamaño acotados.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

pub mod edit;
mod parse;
pub mod write;

use std::collections::BTreeMap;

/// Bloques de primer nivel como mucho (lo demás queda en el último, como texto).
pub const MAX_BLOCKS: usize = 50_000;
/// Anidado máximo (citas y listas); un bloque más profundo se conserva como [`Block::Raw`].
pub const MAX_DEPTH: usize = 24;
/// Documento más grande que se modela (más grande: un solo [`Block::Raw`]).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Estilo de un tramo de texto.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
}

/// Contenido en línea.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text {
        text: String,
        style: Style,
    },
    /// Código en línea (puede ir en cursiva, negrita o tachado: `*`código`*`).
    Code {
        text: String,
        style: Style,
    },
    Link {
        url: String,
        title: String,
        content: Vec<Inline>,
    },
    Image {
        src: String,
        alt: String,
        title: String,
    },
    /// Fórmula en línea (`$…$`).
    Math(String),
    /// Salto de línea forzado.
    Break,
    /// Salto de línea suave (fin de línea dentro del párrafo).
    SoftBreak,
    /// HTML en línea, tal cual.
    Html(String),
}

impl Inline {
    /// Texto sin estilo.
    pub fn text(s: impl Into<String>) -> Inline {
        Inline::Text { text: s.into(), style: Style::default() }
    }
}

/// Alineación de una columna de tabla.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// Elemento de una lista.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// `Some` en las listas de tareas (`- [ ]` / `- [x]`).
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

/// Un bloque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// Título de nivel 1–6.
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    /// `tight`: sin líneas en blanco entre elementos.
    List {
        ordered: bool,
        start: u64,
        tight: bool,
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    /// Aviso o mapa mental de IureEditor: `> [!KIND|meta] título` y su cuerpo.
    Callout {
        kind: String,
        meta: String,
        title: String,
        blocks: Vec<Block>,
    },
    /// Bloque de código (`lang` = `mermaid` para diagramas).
    Code {
        lang: String,
        text: String,
    },
    /// Fórmula en bloque (`$$…$$`).
    Math(String),
    Table {
        align: Vec<Align>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    /// Cabecera YAML (`---` … `---`), sin los separadores.
    FrontMatter(String),
    /// Lo que no se modela: se escribe tal cual.
    Raw(String),
}

/// Metadatos de un bloque (bloques extensibles de la app anfitriona).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// Tipo (`segment`, `task`, `mail`…): letras, números, `-` y `_`.
    pub kind: String,
    /// Atributos (texto).
    pub attrs: BTreeMap<String, String>,
}

/// Un bloque de primer nivel con su texto original.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub block: Block,
    pub meta: Option<Meta>,
    /// Texto original (con el espacio que lo sigue); `None` si se cambió o es nuevo.
    source: Option<String>,
    /// Espacio (saltos de línea) tras el contenido del bloque original; se respeta al reescribirlo.
    trailing: String,
}

impl Node {
    /// ¿Se escribe tal como se leyó?
    pub fn untouched(&self) -> bool {
        self.source.is_some()
    }
}

/// Un documento.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// Lo que va antes del primer bloque (espacio en blanco).
    prefix: String,
    nodes: Vec<Node>,
}

impl Document {
    /// Lee un documento. Nunca falla: lo que no entiende queda como [`Block::Raw`].
    pub fn parse(md: &str) -> Document {
        parse::parse(md)
    }

    /// Los bloques.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Markdown del documento: lo intacto tal cual, lo cambiado desde el modelo.
    pub fn to_markdown(&self) -> String {
        let mut out = String::with_capacity(self.prefix.len() + self.nodes.len() * 64);
        out.push_str(&self.prefix);
        let last = self.nodes.len().saturating_sub(1);
        // ¿La lista anterior se escribió con el marcador alterno?
        let mut prev_alt = false;
        for (i, n) in self.nodes.iter().enumerate() {
            match &n.source {
                Some(s) => {
                    out.push_str(s);
                    // Una lista original: ¿usa el marcador «alterno» (`*` o `1)`)?
                    prev_alt = matches!(n.block, Block::List { .. }) && uses_alt_marker(s);
                }
                None => {
                    // Un bloque reescrito va separado por una línea en blanco: pegado al anterior
                    // podría leerse distinto (un `---` tras un párrafo es un título).
                    if !out.is_empty() && !out.ends_with("\n\n") {
                        out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
                    }
                    if let Some(m) = &n.meta {
                        out.push_str(&write::meta(m));
                        out.push('\n');
                    }
                    // Una lista pegada a otra del mismo tipo usa el otro marcador (no se juntan).
                    let prev = i.checked_sub(1).and_then(|p| self.nodes.get(p)).map(|p| &p.block);
                    let alt = write::merges_with(prev, &n.block) && !prev_alt;
                    out.push_str(&write::block_alt(&n.block, alt));
                    prev_alt = alt;
                    // El espacio que seguía al bloque original (o una línea en blanco).
                    let trailing = match (n.trailing.matches('\n').count(), i == last) {
                        (0, true) => "\n",
                        (_, true) => n.trailing.as_str(),
                        (0 | 1, false) => "\n\n",
                        (_, false) => n.trailing.as_str(),
                    };
                    out.push_str(trailing);
                }
            }
        }
        out
    }

    /// Cambia el bloque `i` (se escribirá desde el modelo). `false` si no existe.
    pub fn set(&mut self, i: usize, block: Block) -> bool {
        match self.nodes.get_mut(i) {
            Some(n) => {
                n.block = block;
                n.source = None;
                true
            }
            None => false,
        }
    }

    /// Cambia (o quita) los metadatos del bloque `i`. `false` si no existe.
    pub fn set_meta(&mut self, i: usize, meta: Option<Meta>) -> bool {
        match self.nodes.get_mut(i) {
            Some(n) => {
                n.meta = meta;
                n.source = None;
                true
            }
            None => false,
        }
    }

    /// Inserta un bloque nuevo en `i` (al final si `i` es mayor).
    pub fn insert(&mut self, i: usize, block: Block, meta: Option<Meta>) {
        if self.nodes.len() >= MAX_BLOCKS {
            return;
        }
        let at = i.min(self.nodes.len());
        // El anterior, si no tenía espacio tras de sí, necesita una línea en blanco.
        if let Some(prev) = at.checked_sub(1).and_then(|p| self.nodes.get_mut(p))
            && let Some(src) = &prev.source
            && !src.ends_with("\n\n")
        {
            let mut s = src.clone();
            s.push_str(if s.ends_with('\n') { "\n" } else { "\n\n" });
            prev.source = Some(s);
        }
        self.nodes.insert(at, Node { block, meta, source: None, trailing: String::new() });
    }

    /// Quita el bloque `i`.
    pub fn remove(&mut self, i: usize) -> Option<Block> {
        (i < self.nodes.len()).then(|| self.nodes.remove(i).block)
    }
}

/// Trozos de primer nivel que cubren el texto entero, en orden: cada uno es un bloque con el espacio
/// que lo sigue (el primero, además, con lo que va antes). Concatenados dan el texto byte a byte;
/// texto vacío, ninguno. Para editores por bloques, que editan cada trozo por separado.
pub fn chunks(md: &str) -> Vec<std::ops::Range<usize>> {
    parse::chunks(md)
}

/// ¿El texto de una lista empieza con el marcador alterno (`*` en viñetas, `1)` en numeradas)?
fn uses_alt_marker(src: &str) -> bool {
    let t = src.trim_start();
    if t.starts_with('*') {
        return true;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && t.get(digits..digits + 1) == Some(")")
}

impl Meta {
    /// `kind` válido (letras, números, `-`, `_`; 1–32).
    pub fn valid_kind(kind: &str) -> bool {
        (1..=32).contains(&kind.len()) && kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }
}

#[cfg(test)]
mod tests;
