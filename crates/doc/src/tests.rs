use super::*;
use proptest::prelude::*;

const NOTE: &str = "---\ntags: [caso]\n---\n# Reunión con *Altamira*\n\nAcuerdos de la **reunión** con [Altamira](https://x.mx).\n\n- [ ] Llamar al notario\n- [x] Enviar propuesta\n  - sub\n\n> [!MINDMAP|map layout-down] Proyecto\n> - Análisis\n> - Diseño\n\n```mermaid\nflowchart LR\n  A-->B\n```\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\n$$\nx^2\n$$\n\n<div>html</div>\n\nTexto[^1].\n\n[^1]: Nota al pie.\n\n***\n";

#[test]
fn untouched_document_is_byte_identical() {
    for md in [NOTE, "", "\n\n", "hola", "  sangría\n", "# t\r\n\r\npárrafo\r\n", "a\n\n\n\nb\n\n\n"] {
        assert_eq!(Document::parse(md).to_markdown(), md, "{md:?}");
    }
}

#[test]
fn blocks_are_modeled() {
    let d = Document::parse(NOTE);
    let b: Vec<&Block> = d.nodes().iter().map(|n| &n.block).collect();
    assert!(matches!(b[0], Block::FrontMatter(y) if y.contains("tags")));
    assert!(matches!(b[1], Block::Heading { level: 1, .. }));
    let Block::List { ordered: false, items, .. } = b[3] else { panic!("{:?}", b[3]) };
    assert_eq!((items[0].checked, items[1].checked), (Some(false), Some(true)));
    let Block::Callout { kind, meta, title, blocks } = b[4] else { panic!("{:?}", b[4]) };
    assert_eq!((kind.as_str(), meta.as_str(), title.as_str()), ("MINDMAP", "map layout-down", "Proyecto"));
    assert!(matches!(blocks.first(), Some(Block::List { .. })));
    assert!(matches!(b[5], Block::Code { lang, text } if lang == "mermaid" && text.contains("A-->B")));
    let Block::Table { align, head, rows } = b[6] else { panic!("{:?}", b[6]) };
    assert_eq!((align.len(), head.len(), rows.len()), (2, 2, 1));
    assert_eq!(align[1], Align::Right);
    assert!(matches!(b[7], Block::Math(m) if m.contains("x^2")));
    assert!(matches!(b[8], Block::Raw(h) if h.contains("<div>")));
    assert!(matches!(b.last(), Some(Block::Rule)));
}

#[test]
fn only_changed_blocks_are_rewritten() {
    let mut d = Document::parse(NOTE);
    let Block::List { ordered, start, tight, mut items } = d.nodes()[3].block.clone() else { panic!() };
    assert!(tight);
    items[0].checked = Some(true);
    assert!(d.set(3, Block::List { ordered, start, tight, items }));
    let out = d.to_markdown();
    // Solo cambió la casilla (y su lista se escribe con el estilo fijo).
    assert_eq!(out, NOTE.replace("- [ ] Llamar", "- [x] Llamar"));
    // Lo demás, idéntico (incluido lo que no se modela).
    assert!(out.contains("<div>html</div>") && out.contains("[^1]: Nota al pie."));
}

#[test]
fn new_blocks_and_metadata() {
    let mut d = Document::parse("# Minuta\n\nIntro.\n");
    let mut attrs = BTreeMap::new();
    attrs.insert("speaker".to_string(), "Ana".to_string());
    attrs.insert("start".to_string(), "00:01:02".to_string());
    let meta = Meta { kind: "segment".into(), attrs };
    d.insert(2, Block::Paragraph(vec![Inline::text("Buenos días.")]), Some(meta.clone()));
    let md = d.to_markdown();
    assert_eq!(
        md,
        "# Minuta\n\nIntro.\n\n<!-- iure:segment {\"speaker\":\"Ana\",\"start\":\"00:01:02\"} -->\nBuenos días.\n"
    );
    // Se vuelve a leer igual: el comentario es el metadato del bloque siguiente.
    let back = Document::parse(&md);
    assert_eq!(back.nodes().len(), 3);
    assert_eq!(back.nodes()[2].meta.as_ref(), Some(&meta));
    assert!(matches!(&back.nodes()[2].block, Block::Paragraph(_)));
    assert_eq!(back.to_markdown(), md);
    // Quitar el metadato reescribe solo ese bloque.
    let mut b2 = back.clone();
    b2.set_meta(2, None);
    assert_eq!(b2.to_markdown(), "# Minuta\n\nIntro.\n\nBuenos días.\n");
    // Un comentario que no es nuestro es un bloque más.
    let other = Document::parse("<!-- nota -->\n\nTexto\n");
    assert_eq!(other.nodes().len(), 2);
    assert!(other.nodes()[1].meta.is_none());
}

#[test]
fn writer_round_trips_through_the_parser() {
    let blocks = vec![
        Block::Heading { level: 2, content: vec![Inline::text("Título # raro")] },
        Block::Paragraph(vec![
            Inline::text("Texto con * y _ y [corchetes] "),
            Inline::Text { text: "negrita".into(), style: Style { bold: true, ..Style::default() } },
            Inline::text(" y "),
            Inline::Code { text: "a`b".into(), style: Style::default() },
            Inline::text(" "),
            Inline::Link {
                url: "https://x.mx/a b".into(),
                title: String::new(),
                content: vec![Inline::text("enlace")],
            },
        ]),
        Block::List {
            ordered: true,
            start: 3,
            tight: true,
            items: vec![
                Item { checked: None, blocks: vec![Block::Paragraph(vec![Inline::text("tres")])] },
                Item { checked: None, blocks: vec![Block::Paragraph(vec![Inline::text("cuatro")])] },
            ],
        },
        Block::Callout { kind: "NOTE".into(), meta: String::new(), title: "Ojo".into(), blocks: vec![] },
        Block::Code { lang: "rust".into(), text: "let a = \"```\";\n".into() },
        Block::Table {
            align: vec![Align::Left, Align::Center],
            head: vec![vec![Inline::text("a|b")], vec![Inline::text("c")]],
            rows: vec![vec![vec![Inline::text("1")], vec![Inline::text("2")]]],
        },
        Block::Rule,
    ];
    let mut d = Document::default();
    for b in &blocks {
        d.insert(usize::MAX, b.clone(), None);
    }
    let md = d.to_markdown();
    let back = Document::parse(&md);
    let got: Vec<Block> = back.nodes().iter().map(|n| n.block.clone()).collect();
    assert_eq!(got.len(), blocks.len(), "{md}");
    for (g, w) in got.iter().zip(&blocks) {
        match (g, w) {
            (Block::Paragraph(_), Block::Paragraph(_)) => {
                assert_eq!(write::block(g), write::block(w), "{md}");
            }
            _ => assert_eq!(g, w, "{md}"),
        }
    }
}

#[test]
fn tight_items_keep_leading_emphasis_and_html() {
    // Regresiones de las pruebas de propiedades: el primer elemento de un elemento apretado.
    for md in ["- *uno* dos\n", "- [enlace](https://x.mx) y más\n", "- <a> texto\n"] {
        let mut d = Document::parse(md);
        let b = d.nodes()[0].block.clone();
        d.set(0, b);
        assert_eq!(d.to_markdown(), md, "{md:?}");
    }
}

#[test]
fn hostile_input_is_bounded() {
    let deep = format!("{}hola\n", "> ".repeat(500));
    let d = Document::parse(&deep);
    assert!(matches!(d.nodes()[0].block, Block::Raw(_)));
    assert_eq!(d.to_markdown(), deep);
    let many: String = (0..MAX_BLOCKS + 10).map(|i| format!("p{i}\n\n")).collect();
    let d = Document::parse(&many);
    assert_eq!(d.nodes().len(), MAX_BLOCKS);
    assert_eq!(d.to_markdown(), many, "lo que sobra se conserva");
}

#[test]
fn chunks_are_the_top_level_blocks() {
    let md = "\n# Título\n\nUno\ndos\n\n- a\n\n- b\n\n```\nx\n\ny\n```\n";
    let parts: Vec<&str> = chunks(md).into_iter().filter_map(|r| md.get(r)).collect();
    assert_eq!(parts, ["\n# Título\n\n", "Uno\ndos\n\n", "- a\n\n- b\n\n", "```\nx\n\ny\n```\n"]);
    assert!(chunks("").is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    /// Leer y escribir sin cambios nunca altera el archivo.
    #[test]
    fn chunks_cover_the_text(md in "(\\PC|#|> |- |\\* |1\\. |```|\\[ \\] |\\| |---|\\$\\$|<!-- iure:x \\{\\} -->|\n|  ){0,300}") {
        let ch = chunks(&md);
        let mut at = 0;
        for r in &ch {
            prop_assert_eq!(r.start, at);
            prop_assert!(r.end > r.start);
            at = r.end;
        }
        prop_assert_eq!(at, md.len());
        let joined: String = ch.iter().filter_map(|r| md.get(r.clone())).collect();
        prop_assert_eq!(joined, md);
    }

    #[test]
    fn round_trip_is_lossless(md in "(\\PC|#|> |- |\\* |1\\. |```|\\[ \\] |\\| |---|\\$\\$|<!-- iure:x \\{\\} -->|\n|  ){0,300}") {
        prop_assert_eq!(Document::parse(&md).to_markdown(), md);
    }

    /// Reescribir todos los bloques da Markdown que se vuelve a leer sin pánico y con los mismos
    /// tipos de bloque.
    #[test]
    fn rewriting_everything_is_stable(md in "(\\PC|#|> |- |1\\. |```|\\[ \\] |\\| |---|\n){0,200}") {
        let mut d = Document::parse(&md);
        for i in 0..d.nodes().len() {
            let b = d.nodes()[i].block.clone();
            d.set(i, b);
        }
        let once = d.to_markdown();
        let again = Document::parse(&once);
        let mut d2 = again.clone();
        for i in 0..d2.nodes().len() {
            let b = d2.nodes()[i].block.clone();
            d2.set(i, b);
        }
        // Escribir lo ya escrito es un punto fijo.
        prop_assert_eq!(d2.to_markdown(), once);
    }
}
