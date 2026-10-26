//! Pruebas de extremo a extremo: corpus, red, enlaces, tiempos y no-pánico.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use iuredesk_html_view::{Bytes, Fetcher, RenderOptions, render, sanitize, to_plain_text};
use proptest::prelude::*;

fn opts_for(html: &str) -> RenderOptions {
    RenderOptions { inline_resources: common::synth_inline_resources(html), ..RenderOptions::default() }
}

#[test]
fn whole_corpus_renders() {
    for case in common::corpus() {
        let mut o = opts_for(&case.html);
        o.inline_resources.extend(case.inline.clone());
        let r = render(&case.html, &o).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        assert_eq!(r.rgba.len(), (r.width * r.height * 4) as usize, "{}", case.name);
        assert!(r.width >= 800 && r.height > 0, "{}", case.name);
        // No es todo de un solo color: algo se pintó.
        let first = &r.rgba[..4];
        assert!(r.rgba.chunks_exact(4).any(|p| p != first), "{} salió en blanco", case.name);
        for l in &r.links {
            assert!(l.rect.y < r.height as f32 && l.rect.x < r.width as f32, "{}: {:?}", case.name, l);
        }
    }
}

#[test]
fn timing_budget() {
    // Presupuesto holgado (las pruebas pueden correr en depuración y en CI lento): lo que importa
    // es detectar regresiones de órdenes de magnitud.
    iuredesk_html_view::warm_up();
    for case in common::corpus() {
        let o = opts_for(&case.html);
        let t = Instant::now();
        let _ = render(&case.html, &o);
        let budget = if case.html.len() > 1_000_000 { Duration::from_secs(60) } else { Duration::from_secs(5) };
        assert!(t.elapsed() < budget, "{} tardó {:?}", case.name, t.elapsed());
    }
}

struct CountingFetcher(AtomicUsize);
impl Fetcher for CountingFetcher {
    fn fetch(&self, _url: &str) -> Option<Bytes> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(Bytes::from(common::synth_png(10, 10, "remota")))
    }
}

fn tracking_html() -> String {
    std::fs::read_to_string(common::corpus_dir().join("15-tracking-pixels.html")).unwrap()
}

#[test]
fn remote_blocked_by_default_and_listed() {
    let fetcher = Arc::new(CountingFetcher(AtomicUsize::new(0)));
    let o = RenderOptions { fetcher: Some(fetcher.clone()), ..RenderOptions::default() };
    let r = render(&tracking_html(), &o).unwrap();
    assert_eq!(fetcher.0.load(Ordering::SeqCst), 0, "no debe pedir nada sin permiso");
    assert!(r.blocked.iter().any(|u| u.contains("tracker.example-esp.com/open")), "{:?}", r.blocked);
    assert!(r.blocked.iter().any(|u| u.contains("campaign-7781.css")), "{:?}", r.blocked);
    assert!(r.blocked.iter().any(|u| u.contains("hero-bg.jpg")), "{:?}", r.blocked);
}

#[test]
fn remote_allowed_goes_through_fetcher() {
    let fetcher = Arc::new(CountingFetcher(AtomicUsize::new(0)));
    let o = RenderOptions { fetcher: Some(fetcher.clone()), allow_remote_images: true, ..RenderOptions::default() };
    let r = render(&tracking_html(), &o).unwrap();
    assert!(fetcher.0.load(Ordering::SeqCst) > 0);
    assert!(r.blocked.is_empty());
}

#[test]
fn links_are_hit_testable() {
    let html = r#"<p style="margin:0;font-size:20px">Hola <a href="https://example.com/x">enlace</a></p>
        <div style="height:50px"></div><a href="mailto:a@b.c" style="display:block;width:200px;height:40px">bloque</a>"#;
    let r = render(html, &RenderOptions { scale: 2.0, ..RenderOptions::default() }).unwrap();
    assert_eq!(r.width, 1600);
    let inline = r.links.iter().find(|l| l.href == "https://example.com/x").expect("enlace en línea");
    let (cx, cy) = (inline.rect.x + inline.rect.width / 2.0, inline.rect.y + inline.rect.height / 2.0);
    assert_eq!(r.link_at(cx, cy), Some("https://example.com/x"));
    assert!(inline.rect.x > 40.0, "el enlace va después de «Hola»: {:?}", inline.rect);
    let block = r.links.iter().find(|l| l.href == "mailto:a@b.c").expect("enlace de bloque");
    assert!((block.rect.width - 400.0).abs() < 2.0, "{:?}", block.rect);
    assert_eq!(r.link_at(1.0, 1.0), None);
}

#[test]
fn dark_mode_only_when_supported() {
    let plain = render("<p>hola</p>", &RenderOptions { dark_mode: true, ..RenderOptions::default() }).unwrap();
    assert!(!plain.dark_applied);
    assert_eq!(&plain.rgba[..3], &[255, 255, 255]);
    let html = std::fs::read_to_string(common::corpus_dir().join("05-dark-mode-meta.html")).unwrap();
    let dark = render(&html, &RenderOptions { dark_mode: true, ..RenderOptions::default() }).unwrap();
    assert!(dark.dark_applied);
    assert!(dark.rgba[0] < 64, "fondo oscuro esperado");
}

#[test]
fn height_is_capped() {
    let r = render(&common::huge_email(), &RenderOptions { max_height: 3000, ..RenderOptions::default() }).unwrap();
    assert!(r.truncated);
    assert_eq!(r.height, 3000);
}

#[test]
fn rejects_bad_options_and_huge_input() {
    assert!(render("x", &RenderOptions { viewport_width: 0, ..RenderOptions::default() }).is_err());
    assert!(render("x", &RenderOptions { scale: f32::NAN, ..RenderOptions::default() }).is_err());
    assert!(render("xx", &RenderOptions { max_input_bytes: 1, ..RenderOptions::default() }).is_err());
}

/// Regresión: Blitz entraba en pánico con URLs relativas porque el documento no tenía base.
#[test]
fn relative_urls_do_not_fail() {
    let html = r#"<img src="foto.png" width="10" height="10"><link rel="stylesheet" href="estilo.css">
        <div style="background:url(fondo.jpg)">x</div><a href="pagina.html">rel</a>"#;
    let r = render(html, &RenderOptions::default()).expect("render con URLs relativas");
    assert!(r.blocked.is_empty(), "las relativas no son remotas: {:?}", r.blocked);
}

#[test]
fn empty_input_does_not_crash() {
    for html in ["", " ", "<!-- -->", "<html></html>", "\0\0", "<script>x</script>"] {
        let _ = render(html, &RenderOptions::default());
    }
}

#[test]
fn plain_text_of_corpus() {
    for case in common::corpus() {
        let t = to_plain_text(&case.html);
        assert!(!t.contains("<script"), "{}", case.name);
        assert!(!t.contains("SOLO OUTLOOK"), "{}: condicional MSO filtrado", case.name);
    }
}

// --- Propiedades -----------------------------------------------------------------------------

/// Retokeniza la salida del saneador y busca cualquier cosa ejecutable.
#[derive(Default)]
struct Checker {
    bad: RefCell<Vec<String>>,
}
impl TokenSink for Checker {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        if let Token::TagToken(tag) = token {
            let name: &str = &tag.name;
            if matches!(name, "script" | "iframe" | "object" | "embed" | "base" | "frame" | "applet") {
                self.bad.borrow_mut().push(format!("<{name}>"));
            }
            for a in &tag.attrs {
                let an: String = a.name.local.to_ascii_lowercase().to_string();
                let v: String = a
                    .value
                    .chars()
                    .filter(|c| !c.is_whitespace() && !c.is_control())
                    .collect::<String>()
                    .to_ascii_lowercase();
                if an.starts_with("on") || an == "srcdoc" || an == "formaction" {
                    self.bad.borrow_mut().push(an.clone());
                }
                if (an.ends_with("href") || an == "src" || an == "action" || an == "background")
                    && (v.starts_with("javascript:") || v.starts_with("vbscript:") || v.starts_with("data:text"))
                {
                    self.bad.borrow_mut().push(format!("{an}={v}"));
                }
            }
            if name == "meta"
                && tag.attrs.iter().any(|a| &*a.name.local == "http-equiv" && a.value.eq_ignore_ascii_case("refresh"))
            {
                self.bad.borrow_mut().push("meta refresh".into());
            }
            // Mantener el mismo estado de texto crudo que el saneador.
            return match name {
                "style" | "xmp" => TokenSinkResult::RawData(html5ever::tokenizer::states::RawKind::Rawtext),
                "title" | "textarea" => TokenSinkResult::RawData(html5ever::tokenizer::states::RawKind::Rcdata),
                _ => TokenSinkResult::Continue,
            };
        }
        TokenSinkResult::Continue
    }
}

fn dangerous_bits(html: &str) -> Vec<String> {
    let tok = Tokenizer::new(Checker::default(), TokenizerOpts::default());
    let q = BufferQueue::default();
    q.push_back(StrTendril::from_slice(html));
    let _ = tok.feed(&q);
    tok.end();
    tok.sink.bad.into_inner()
}

fn html_soup() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("<script>alert(1)</script>".to_owned()),
        Just("<SCRIPT>".to_owned()),
        Just("</script>".to_owned()),
        Just("<img src=x onerror=alert(1)>".to_owned()),
        Just("<a href=\"java\tscript:x\">".to_owned()),
        Just("<a href='&#106;avascript:x'>".to_owned()),
        Just("<iframe src=//e>".to_owned()),
        Just("<style>".to_owned()),
        Just("</style>".to_owned()),
        Just("<svg><use href=/etc/passwd>".to_owned()),
        Just("<table><tr><td>".to_owned()),
        Just("</td></tr></table>".to_owned()),
        Just("<!--[if mso]>".to_owned()),
        Just("<![endif]-->".to_owned()),
        Just("<title>".to_owned()),
        Just("<textarea>".to_owned()),
        Just("<plaintext>".to_owned()),
        Just("<noscript>".to_owned()),
        Just("<math><mtext><table><mglyph><style>".to_owned()),
        Just("<meta http-equiv=refresh content=0>".to_owned()),
        Just("<p title=\"".to_owned()),
        Just("\"".to_owned()),
        Just("'".to_owned()),
        Just(">".to_owned()),
        Just("<".to_owned()),
        Just("&#".to_owned()),
        Just("\u{0}".to_owned()),
        "[a-z <>/=\"']{0,12}",
        "\\PC{0,8}",
    ];
    prop::collection::vec(piece, 0..40).prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn sanitizer_output_has_nothing_executable(html in html_soup()) {
        let out = sanitize(&html).html;
        let bad = dangerous_bits(&out);
        prop_assert!(bad.is_empty(), "entrada {html:?}\nsalida {out:?}\nmalo {bad:?}");
    }

    #[test]
    fn plain_text_never_panics(html in "\\PC{0,400}") {
        let _ = to_plain_text(&html);
        let _ = sanitize(&html);
    }
}

proptest! {
    // El render completo es caro: menos casos.
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn render_never_panics(html in html_soup(), width in 50u32..1200, scale in prop::sample::select(vec![0.5f32, 1.0, 1.5, 2.0])) {
        let o = RenderOptions { viewport_width: width, scale, max_height: 4000, ..RenderOptions::default() };
        // Debe devolver Ok o Err, nunca abortar el proceso.
        let _ = render(&html, &o);
    }

    #[test]
    fn render_garbled_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2000)) {
        let html = String::from_utf8_lossy(&bytes);
        let _ = render(&html, &RenderOptions { max_height: 2000, ..RenderOptions::default() });
    }
}
