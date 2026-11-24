//! S1.36: noticias en la cabecera de «Mi día», entre el saludo y el clima (debajo del saludo si la
//! ventana es angosta). De cada fuente, sus últimos 1 a 3 artículos: ▶ en los que tienen audio
//! (suena aunque se cambie de módulo); el título abre el navegador. El engrane abre «Fuentes de
//! noticias»: cuántos artículos, quitar, agregar o restaurar las de omisión.
//!
//! Los datos los trae el motor ([`iuredesk_engine::news`]); el sonido, [`iuredesk_audio`].

use std::time::Instant;

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_audio::{AudioError, Player, State};
use iuredesk_engine::lang::Text;
use iuredesk_engine::news::{self, NewsAudio, NewsItem, NewsResult, NewsSource, SourceNews, Topic};
use iurefficient_ui::{Icon, icons, pick, theme};

use crate::app::DeskApp;

/// Estado de las noticias.
#[derive(Default)]
pub struct NewsUi {
    data: Vec<SourceNews>,
    /// Fuentes de `data` (o del pedido en curso).
    for_sources: Option<Vec<NewsSource>>,
    fetching: bool,
    last: Option<Instant>,
    failed: bool,
    pub(crate) player: Player,
    /// Artículo que suena (o sonó) en `player`.
    playing: Option<NewsItem>,
    /// Artículo cuyo audio se está trayendo.
    loading: Option<String>,
    /// Error del último ▶ (enlace del artículo y texto).
    error: Option<(String, Text)>,
    /// Diálogo «Fuentes de noticias».
    pub(crate) setup_open: bool,
    new_name: String,
    new_url: String,
    new_error: Option<Text>,
}

/// Pide las noticias si hace falta (fuentes nuevas o pasó el intervalo).
fn ensure(app: &mut DeskApp) {
    let pref = app.session.prefs.news.clone();
    if !pref.enabled {
        app.today.news.data.clear();
        return;
    }
    let n = &app.today.news;
    if n.fetching {
        return;
    }
    // Solo cambió el orden: se reacomoda lo que ya hay, sin volver a pedirlo.
    if let Some(old) = &n.for_sources
        && *old != pref.sources
        && old.len() == pref.sources.len()
        && pref.sources.iter().all(|s| old.contains(s))
    {
        let n = &mut app.today.news;
        let mut data = std::mem::take(&mut n.data);
        n.data = pref
            .sources
            .iter()
            .filter_map(|s| data.iter().position(|d| d.source == *s).map(|i| data.swap_remove(i)))
            .collect();
        n.for_sources = Some(pref.sources);
        return;
    }
    let wait = if n.failed { news::RETRY } else { news::REFRESH };
    let due = n.last.is_none_or(|t| t.elapsed() >= wait);
    if n.for_sources.as_ref() != Some(&pref.sources) || due {
        let n = &mut app.today.news;
        n.for_sources = Some(pref.sources);
        n.fetching = true;
        n.last = Some(Instant::now());
        if app.session.request_news().is_none() {
            app.today.news.fetching = false;
        }
    }
}

/// Llegaron las noticias.
pub fn on_news(app: &mut DeskApp, r: NewsResult) {
    let n = &mut app.today.news;
    if n.for_sources.as_ref() != Some(&r.for_sources) {
        return;
    }
    n.fetching = false;
    n.failed = r.news.iter().any(|s| s.result.is_err());
    n.data = r.news;
}

/// Llegó (o no) el audio de un artículo: suena.
pub fn on_audio(app: &mut DeskApp, r: NewsAudio) {
    let n = &mut app.today.news;
    if n.loading.as_deref() != Some(r.link.as_str()) {
        return;
    }
    n.loading = None;
    match r.result {
        Ok(path) => {
            n.error = None;
            n.player.play(path);
        }
        Err(t) => {
            n.playing = None;
            n.error = Some((r.link, t));
        }
    }
}

fn audio_error(e: AudioError) -> Text {
    match e {
        AudioError::Open | AudioError::Format => {
            Text::new("This audio couldn't be played.", "No se pudo reproducir este audio.")
        }
        AudioError::NoOutput => Text::new(
            "No sound output is available (is the sound server running?).",
            "No hay salida de sonido disponible (¿está activo el servidor de sonido?).",
        ),
    }
}

/// ▶ de un artículo: si es el que suena, pausa o sigue; si no, lo trae y lo toca.
fn toggle(app: &mut DeskApp, item: &NewsItem) {
    let n = &mut app.today.news;
    let same = n.playing.as_ref().is_some_and(|p| p.link == item.link);
    let st = n.player.status().state;
    if same && matches!(st, State::Playing | State::Paused) {
        n.player.toggle_pause();
        return;
    }
    n.player.stop();
    n.playing = Some(item.clone());
    n.loading = Some(item.link.clone());
    n.error = None;
    app.session.request_news_audio(item);
}

/// S1.36: comandos `news.play_pause` y `news.stop` (paleta, atajos).
pub fn command(app: &mut DeskApp, id: &str) {
    match id {
        "news.stop" => {
            app.today.news.player.stop();
            app.today.news.loading = None;
        }
        _ => {
            let current = app.today.news.playing.clone();
            let first = || {
                app.today.news.data.iter().find_map(|s| s.result.as_ref().ok()?.iter().find(|i| i.has_audio()).cloned())
            };
            if let Some(item) = current.or_else(first) {
                toggle(app, &item);
            }
        }
    }
}

fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Botón redondo ▶ / ⏸ (o girando mientras llega el audio).
fn play_button(ui: &mut Ui, playing: bool, loading: bool, tip: &str) -> egui::Response {
    let c = theme::colors(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    iurefficient_ui::a11y::button(&resp, tip);
    let p = ui.painter();
    let fill = if resp.hovered() { c.accent } else { c.accent.gamma_multiply(0.9) };
    p.circle_filled(rect.center(), 13.0, fill);
    let ink = Color32::WHITE;
    if loading {
        let t = ui.input(|i| i.time) as f32;
        let r = 6.0;
        let pts: Vec<_> = (0..=20)
            .map(|k| {
                let a = t * 6.0 + k as f32 * 0.22;
                rect.center() + vec2(a.cos() * r, a.sin() * r)
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0, ink)));
        ui.ctx().request_repaint();
    } else if playing {
        for dx in [-3.0, 3.0] {
            let bar = Rect::from_center_size(rect.center() + vec2(dx, 0.0), vec2(3.0, 11.0));
            p.rect_filled(bar, 1.0, ink);
        }
    } else {
        let m = rect.center() + vec2(1.5, 0.0);
        p.add(egui::Shape::convex_polygon(
            vec![m + vec2(-4.5, -6.0), m + vec2(6.0, 0.0), m + vec2(-4.5, 6.0)],
            ink,
            Stroke::NONE,
        ));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tip)
}

/// Título que abre el artículo en el navegador. Devuelve si se pulsó.
fn title_link(ui: &mut Ui, text: &str, ink: Color32, small: bool, hover: &str) -> bool {
    let font = if small { theme::small_font(ui.ctx()) } else { theme::body_font(ui.ctx()) };
    let r = ui.add(egui::Label::new(RichText::new(text).font(font).color(ink)).truncate().sense(Sense::click()));
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    r.on_hover_text(hover).clicked()
}

/// ¿Se muestran noticias (para reservarles sitio en la cabecera)?
pub fn shown(app: &DeskApp) -> bool {
    app.session.prefs.news.enabled && !app.session.prefs.news.sources.is_empty()
}

/// Las noticias: de cada fuente sus últimos artículos; ▶ solo en los que tienen audio. El tema
/// («Noticias generales») va antes del primero de cada fuente; la fuente se ve en «Fuentes…» y
/// al pasar el ratón.
pub fn panel(app: &mut DeskApp, ui: &mut Ui) {
    ensure(app);
    if !shown(app) {
        return;
    }
    let c = theme::colors(ui);
    let status = app.today.news.player.status();
    if matches!(status.state, State::Playing) || app.today.news.loading.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    let data = app.today.news.data.clone();
    let count = app.session.prefs.news.count();
    let mut press: Option<NewsItem> = None;
    let mut seek: Option<f64> = None;
    let mut setup = false;
    let mut open: Option<String> = None;
    let mut first_row = true;
    // El engrane de «Fuentes…» va en la primera fila, a la derecha.
    let mut gear = |ui: &mut Ui, first: &mut bool| {
        if std::mem::take(first)
            && icons::icon_button(ui, Icon::Settings, pick("News sources…", "Fuentes de noticias…"), true).clicked()
        {
            setup = true;
        }
    };
    if data.is_empty() {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                gear(ui, &mut first_row);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if app.today.news.fetching {
                        ui.add(egui::Spinner::new().size(12.0));
                    }
                    ui.label(RichText::new(pick("Loading news…", "Cargando noticias…")).color(c.text_muted));
                });
            });
        });
    }
    for s in &data {
        let items = match &s.result {
            Err(t) => {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        gear(ui, &mut first_row);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(t.get()).small().color(c.text_muted)).truncate());
                        });
                    });
                });
                continue;
            }
            Ok(items) => news::latest(items, count),
        };
        for a in items {
            let mine = app.today.news.playing.as_ref().is_some_and(|p| p.link == a.link);
            let loading = app.today.news.loading.as_deref() == Some(a.link.as_str());
            let on = mine && status.state == State::Playing;
            let hover = format!("{} · {}", s.source.name, pick("open in your browser", "abrir en el navegador"));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if a.has_audio() {
                    let tip = if on {
                        pick("Pause", "Pausar").to_string()
                    } else {
                        format!("{} {}", pick("Listen:", "Escuchar:"), a.title)
                    };
                    if play_button(ui, on, loading, &tip).clicked() {
                        press = Some(a.clone());
                    }
                } else {
                    let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                    let ir = Rect::from_center_size(r.center(), vec2(13.0, 13.0));
                    icons::paint(ui.painter(), ir, Icon::Hyperlink, c.text_muted, c.text_muted);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    gear(ui, &mut first_row);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if title_link(ui, &a.title, c.text, false, &hover) {
                            open = Some(a.link.clone());
                        }
                    });
                });
            });
            if mine
                && matches!(status.state, State::Playing | State::Paused)
                && let Some(to) = progress(ui, status.position, status.duration)
            {
                seek = Some(to);
            }
            if mine && let State::Failed(e) = status.state {
                ui.label(RichText::new(audio_error(e).get()).small().color(c.danger));
            }
            if let Some((link, t)) = &app.today.news.error
                && *link == a.link
            {
                ui.label(RichText::new(t.get()).small().color(c.danger));
            }
        }
    }
    if let Some(item) = press {
        toggle(app, &item);
    }
    if let Some(to) = seek {
        app.today.news.player.seek(to);
    }
    if setup {
        open_setup(app);
    }
    if let Some(link) = open
        && !crate::open_url(&link)
    {
        app.notice(iuredesk_engine::Notice::error(Text::new(
            "Couldn't open the article in your browser.",
            "No se pudo abrir el artículo en el navegador.",
        )));
    }
}

/// Barra de avance con el tiempo; un clic salta. Devuelve a dónde saltar.
fn progress(ui: &mut Ui, pos: f64, dur: Option<f64>) -> Option<f64> {
    let c = theme::colors(ui);
    let mut to = None;
    ui.horizontal(|ui| {
        ui.add_space(34.0);
        let label = match dur {
            Some(d) => format!("{} / {}", clock(pos), clock(d)),
            None => clock(pos),
        };
        let g = ui.painter().layout_no_wrap(label, theme::small_font(ui.ctx()), c.text_muted);
        let w = (ui.available_width() - g.size().x - 12.0).max(40.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 14.0), Sense::click());
        let track = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
        ui.painter().rect_filled(track, 2.0, c.separator);
        if let Some(d) = dur.filter(|d| *d > 0.0) {
            let k = (pos / d).clamp(0.0, 1.0) as f32;
            let done = Rect::from_min_max(track.min, pos2(track.left() + track.width() * k, track.bottom()));
            ui.painter().rect_filled(done, 2.0, c.accent);
            ui.painter().circle_filled(pos2(done.right(), track.center().y), 5.0, c.accent);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                let k = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
                to = Some(f64::from(k) * d);
            }
        }
        iurefficient_ui::a11y::button(&resp, pick("Audio position", "Posición del audio"));
        ui.add_space(6.0);
        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(r.min, g, c.text_muted);
    });
    to
}

/// Abre el diálogo de fuentes.
pub fn open_setup(app: &mut DeskApp) {
    let n = &mut app.today.news;
    n.setup_open = true;
    n.new_name.clear();
    n.new_url.clear();
    n.new_error = None;
}

fn save(app: &mut DeskApp, f: impl FnOnce(&mut news::NewsPref)) {
    f(&mut app.session.prefs.news);
    app.session.save_prefs();
}

/// «Fuentes de noticias»: mostrar o no, quitar, agregar (sitio WordPress o feed) y restaurar.
pub fn setup_dialog(app: &mut DeskApp, ctx: &egui::Context) {
    if !app.today.news.setup_open {
        return;
    }
    let pref = app.session.prefs.news.clone();
    let mut enabled = pref.enabled;
    let mut per_source = pref.count() as u8;
    let mut remove: Option<usize> = None;
    let mut swap: Option<(usize, usize)> = None;
    let mut add = false;
    let mut restore = false;
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::unique("news-setup")).show(ctx, |ui| {
        ui.set_width(460.0);
        let c = theme::colors(ui);
        ui.label(
            RichText::new(pick("News sources", "Fuentes de noticias")).font(theme::bold_font(ui.ctx(), 17.0)).color(c.text),
        );
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(pick(
                    "Each source shows its latest articles; the ones with audio have ▶ to listen to them. A source is a WordPress site or an RSS/Atom feed (podcasts too).",
                    "Cada fuente muestra sus últimos artículos; los que tienen audio llevan ▶ para escucharlos. Una fuente es un sitio WordPress o un feed RSS/Atom (también pódcasts).",
                ))
                .color(c.text_muted),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        let r = ui.checkbox(&mut enabled, pick("Show news in My day", "Mostrar noticias en «Mi día»"));
        crate::widgets::named(&r, egui::Role::CheckBox, pick("Show news", "Mostrar noticias"));
        ui.horizontal(|ui| {
            ui.label(pick("Articles per source:", "Artículos por fuente:"));
            for n in 1..=news::MAX_PER_SOURCE {
                let r = ui.selectable_label(per_source == n, n.to_string());
                crate::widgets::named(
                    &r,
                    egui::Role::Button,
                    &format!("{n} {}", pick("per source", "por fuente")),
                );
                if r.clicked() {
                    per_source = n;
                }
            }
        });
        ui.add_space(6.0);
        // El orden de la lista es el de «Mi día»: ▲/▼ mueven una fuente.
        let last = pref.sources.len().saturating_sub(1);
        for (i, s) in pref.sources.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&s.name).strong().color(c.text));
                if s.topic != Topic::Other {
                    ui.label(RichText::new(s.topic.label()).small().color(c.text_muted));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::icon_button(ui, Icon::Close, &format!("{} {}", pick("Remove", "Quitar"), s.name), true)
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    let tip = format!("{} {}", pick("Move down", "Bajar"), s.name);
                    if icons::icon_button(ui, Icon::ChevronDown, &tip, i < last).clicked() {
                        swap = Some((i, i.saturating_add(1)));
                    }
                    let tip = format!("{} {}", pick("Move up", "Subir"), s.name);
                    if icons::icon_button(ui, Icon::ChevronUp, &tip, i > 0).clicked() {
                        swap = Some((i.saturating_sub(1), i));
                    }
                    ui.add(egui::Label::new(RichText::new(&s.url).small().color(c.text_muted)).truncate());
                });
            });
        }
        ui.add_space(8.0);
        let n = &mut app.today.news;
        if pref.sources.len() < news::MAX_SOURCES {
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut n.new_name).desired_width(110.0).hint_text(pick("Name", "Nombre")),
                );
                crate::widgets::named(&r, egui::Role::TextInput, pick("Source name", "Nombre de la fuente"));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut n.new_url)
                        .desired_width(ui.available_width() - 80.0)
                        .hint_text("https://…"),
                );
                crate::widgets::named(&r, egui::Role::TextInput, pick("Source address", "Dirección de la fuente"));
                if ui.button(pick("Add", "Agregar")).clicked() {
                    add = true;
                }
            });
            if let Some(e) = &n.new_error {
                ui.label(RichText::new(e.get()).small().color(c.danger));
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button(pick("Restore defaults", "Restaurar las de omisión")).clicked() {
                restore = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if iurefficient_ui::primary_button(ui, pick("Done", "Listo")).clicked() {
                    close = true;
                }
            });
        });
    });
    if resp.should_close() {
        close = true;
    }
    if enabled != pref.enabled {
        save(app, |p| p.enabled = enabled);
        if !enabled {
            app.today.news.player.stop();
        }
    }
    if per_source != pref.per_source {
        save(app, |p| p.per_source = per_source);
    }
    if let Some((a, b)) = swap {
        save(app, |p| {
            if a < b && b < p.sources.len() {
                p.sources.swap(a, b);
            }
        });
    }
    if let Some(i) = remove {
        save(app, |p| {
            if i < p.sources.len() {
                p.sources.remove(i);
            }
        });
    }
    if add {
        let n = &mut app.today.news;
        let url = n.new_url.trim().to_string();
        match news::parse_url(&url) {
            Some(u) => {
                let name = match n.new_name.trim() {
                    "" => u.host_str().unwrap_or("").trim_start_matches("www.").to_string(),
                    s => s.chars().take(40).collect(),
                };
                n.new_name.clear();
                n.new_url.clear();
                n.new_error = None;
                save(app, |p| {
                    p.sources.push(NewsSource { name, url, topic: Topic::Other, audio_category: None });
                    p.enabled = true;
                });
            }
            None => {
                n.new_error = Some(Text::new(
                    "Write the full address, starting with https://",
                    "Escribe la dirección completa, empezando por https://",
                ));
            }
        }
    }
    if restore {
        save(app, |p| {
            p.sources = news::default_sources();
            p.enabled = true;
        });
    }
    if close {
        app.today.news.setup_open = false;
    }
}

impl DeskApp {
    /// S1.36: el artículo que suena (pruebas).
    pub fn news_playing(&self) -> Option<(String, State)> {
        self.today.news.playing.as_ref().map(|p| (p.link.clone(), self.today.news.player.status().state))
    }

    /// S1.36: las fuentes en el orden en que se ven (pruebas).
    pub fn today_news_order(&self) -> Vec<String> {
        self.today.news.data.iter().map(|s| s.source.name.clone()).collect()
    }

    /// S1.36: el último error de ▶ (pruebas).
    pub fn news_error(&self) -> Option<String> {
        self.today.news.error.as_ref().map(|(_, t)| t.get().to_string())
    }
}
