//! UX-15: el clima de hoy a la derecha del saludo de «Mi día»: icono, temperatura ahora, máxima y
//! mínima. Un clic abre la previsión en yr.no (la fuente: MET Norway); el menú contextual cambia
//! la ciudad, la unidad o lo quita. Mientras no esté activado se ofrece con un enlace discreto
//! («Ver el clima») que abre un diálogo para elegir la ciudad; nada sale a la red antes.
//!
//! Los datos los trae el motor ([`iuredesk_engine::weather`]); los iconos se dibujan aquí (las
//! fuentes de emoji no están en todos los sistemas).

use std::time::Instant;

use egui::{Align, Color32, Layout, Painter, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_engine::lang::Text;
use iuredesk_engine::weather::{self, DayWeather, Place, PlaceResults, Sky, WeatherResult};
use iurefficient_ui::{Icon, icons, pick, theme};

use crate::app::DeskApp;

/// Estado del clima en la pantalla.
#[derive(Default)]
pub struct WeatherUi {
    data: Option<DayWeather>,
    /// Lugar de `data` (o del pedido en curso).
    for_place: Option<Place>,
    fetching: bool,
    last: Option<Instant>,
    error: Option<Text>,
    /// Diálogo «Clima de hoy».
    pub(crate) setup_open: bool,
    query: String,
    results: Vec<Place>,
    searching: bool,
    search_error: Option<Text>,
    searched: bool,
}

/// Pide el pronóstico si hace falta (lugar nuevo, día nuevo o pasó el intervalo).
fn ensure(app: &mut DeskApp) {
    let pref = app.session.prefs.weather.clone();
    let Some(place) = pref.place.filter(|_| pref.enabled) else {
        app.today.weather.data = None;
        return;
    };
    let w = &app.today.weather;
    if w.fetching {
        return;
    }
    let stale_day = w.data.as_ref().is_some_and(|d| d.date != app.session.today());
    let wait = if w.error.is_some() { weather::RETRY } else { weather::REFRESH };
    let due = w.last.is_none_or(|t| t.elapsed() >= wait);
    if w.for_place.as_ref() != Some(&place) || stale_day || due {
        let w = &mut app.today.weather;
        if w.for_place.as_ref() != Some(&place) {
            w.data = None;
            w.error = None;
        }
        w.for_place = Some(place);
        w.fetching = true;
        w.last = Some(Instant::now());
        if app.session.request_weather().is_none() {
            app.today.weather.fetching = false;
        }
    }
}

/// Llegó el pronóstico.
pub fn on_weather(app: &mut DeskApp, r: WeatherResult) {
    let w = &mut app.today.weather;
    if w.for_place.as_ref() != Some(&r.for_place) {
        return;
    }
    w.fetching = false;
    match r.result {
        Ok(d) => {
            w.data = Some(d);
            w.error = None;
        }
        Err(t) => w.error = Some(t),
    }
}

/// Llegaron ciudades.
pub fn on_places(app: &mut DeskApp, r: PlaceResults) {
    let w = &mut app.today.weather;
    if r.query != w.query.trim().chars().take(80).collect::<String>() {
        return;
    }
    w.searching = false;
    w.searched = true;
    match r.result {
        Ok(p) => {
            w.results = p;
            w.search_error = None;
        }
        Err(t) => {
            w.results.clear();
            w.search_error = Some(t);
        }
    }
}

/// Abre el diálogo para elegir la ciudad (con la sugerencia de la zona horaria).
pub fn open_setup(app: &mut DeskApp) {
    let w = &mut app.today.weather;
    w.setup_open = true;
    w.results.clear();
    w.search_error = None;
    w.searched = false;
    w.query = app
        .session
        .prefs
        .weather
        .place
        .as_ref()
        .map(|p| p.name.clone())
        .or_else(|| app.session.guess_city())
        .unwrap_or_default();
    if !w.query.trim().is_empty() {
        search(app);
    }
}

fn search(app: &mut DeskApp) {
    let q = app.today.weather.query.clone();
    if q.trim().is_empty() {
        return;
    }
    app.today.weather.searching = true;
    app.session.search_places(&q);
}

fn save(app: &mut DeskApp, f: impl FnOnce(&mut weather::WeatherPref)) {
    f(&mut app.session.prefs.weather);
    app.session.save_prefs();
}

/// El clima (o el enlace para activarlo), alineado a la derecha del saludo.
pub fn widget(app: &mut DeskApp, ui: &mut Ui) {
    ensure(app);
    let c = theme::colors(ui);
    let pref = app.session.prefs.weather.clone();
    let small = theme::density(ui.ctx()).small_size();
    let Some(place) = pref.place.clone().filter(|_| pref.enabled) else {
        if pref.dismissed {
            return;
        }
        let r = ui.add(
            egui::Button::new(
                RichText::new(pick("See today's weather", "Ver el clima")).color(c.text_muted).size(small),
            )
            .frame_when_inactive(false),
        );
        if r.on_hover_text(pick("Show today's weather here", "Mostrar aquí el clima de hoy")).clicked() {
            open_setup(app);
        }
        return;
    };
    let f = pref.fahrenheit;
    let w = &app.today.weather;
    let resp = if let Some(d) = w.data.clone() {
        let now = format!("{}°", weather::degrees(d.now_c, f));
        let range = format!("{}° / {}°", weather::degrees(d.max_c, f), weather::degrees(d.min_c, f));
        // El padre va de derecha a izquierda: primero lo de más a la derecha.
        let a = ui
            .add(
                egui::Label::new(RichText::new(format!("{range} · {}", place.name)).color(c.text_muted).size(small))
                    .selectable(false),
            )
            .rect;
        let b = ui.label(RichText::new(now).font(theme::semibold_font(ui.ctx(), 22.0)).color(c.text)).rect;
        let (ir, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
        paint_sky(ui.painter(), ir, d.sky, d.night, c.text_muted, c.warning, c.accent);
        let r = ui.interact(a.union(b).union(ir), egui::Id::unique("weather-now"), Sense::click());
        let tip = format!(
            "{} · {}\n{}",
            d.sky.label(),
            place.full_name(),
            pick("Data: MET Norway. Click to open yr.no.", "Datos: MET Norway. Clic para abrir yr.no.")
        );
        let r = r.on_hover_text(tip);
        crate::widgets::named(
            &r,
            egui::Role::Link,
            &format!("{} {}° {}", pick("Weather:", "Clima:"), weather::degrees(d.now_c, f), d.sky.label()),
        );
        r
    } else if let Some(e) = w.error.clone() {
        let r = ui.add(
            egui::Button::new(
                RichText::new(pick("Weather unavailable", "Clima no disponible")).color(c.text_muted).size(small),
            )
            .frame_when_inactive(false),
        );
        r.on_hover_text(e.get())
    } else {
        ui.add(egui::Spinner::new().size(14.0)).on_hover_text(pick("Getting the weather…", "Obteniendo el clima…"))
    };
    if resp.clicked() {
        if app.today.weather.data.is_some() {
            if !crate::open_url(&place.source_url()) {
                app.notice(iuredesk_engine::Notice::error(Text::new(
                    "Could not open the link.",
                    "No se pudo abrir el enlace.",
                )));
            }
        } else {
            // Reintentar ya.
            app.today.weather.last = None;
            app.today.weather.error = None;
        }
    }
    let mut act: Option<u8> = None;
    resp.context_menu(|ui| {
        if ui.button(pick("Change city…", "Cambiar de ciudad…")).clicked() {
            act = Some(0);
        }
        let unit = if f { pick("Use °C", "Usar °C") } else { pick("Use °F", "Usar °F") };
        if ui.button(unit).clicked() {
            act = Some(1);
        }
        if ui.button(pick("Hide the weather", "Quitar el clima")).clicked() {
            act = Some(2);
        }
    });
    match act {
        Some(0) => open_setup(app),
        Some(1) => save(app, |w| w.fahrenheit = !w.fahrenheit),
        Some(2) => save(app, |w| {
            w.enabled = false;
            w.dismissed = true;
        }),
        _ => {}
    }
}

/// Diálogo «Clima de hoy»: buscar y elegir la ciudad.
pub fn setup_dialog(app: &mut DeskApp, ctx: &egui::Context) {
    if !app.today.weather.setup_open {
        return;
    }
    let mut chosen: Option<Place> = None;
    let mut close = false;
    let mut never = false;
    let mut go = false;
    let resp = egui::Modal::new(egui::Id::unique("weather-setup")).show(ctx, |ui| {
        ui.set_width(420.0);
        let c = theme::colors(ui);
        ui.label(RichText::new(pick("Today's weather", "Clima de hoy")).font(theme::bold_font(ui.ctx(), 17.0)).color(c.text));
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(pick(
                    "Choose your city. The forecast comes from MET Norway (yr.no) and is asked only with the city's coordinates; the city search uses Open-Meteo.",
                    "Elige tu ciudad. El pronóstico es de MET Norway (yr.no) y se pide solo con las coordenadas de la ciudad; la búsqueda de ciudades usa Open-Meteo.",
                ))
                .color(c.text_muted),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        let w = &mut app.today.weather;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut w.query)
                    .desired_width(ui.available_width() - 90.0)
                    .hint_text(pick("City", "Ciudad")),
            );
            crate::widgets::named(&r, egui::Role::TextInput, pick("City", "Ciudad"));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if ui.button(pick("Search", "Buscar")).clicked() {
                go = true;
            }
        });
        ui.add_space(6.0);
        if w.searching {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(RichText::new(pick("Searching…", "Buscando…")).color(c.text_muted));
            });
        } else if let Some(e) = &w.search_error {
            ui.label(RichText::new(e.get()).color(c.danger));
        } else if w.searched && w.results.is_empty() {
            ui.label(RichText::new(pick("No city with that name.", "No hay ninguna ciudad con ese nombre.")).color(c.text_muted));
        }
        for p in &w.results {
            if ui.add(egui::Button::new(p.full_name()).frame_when_inactive(false)).clicked() {
                chosen = Some(p.clone());
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                close = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new(pick("Don't show the weather", "No mostrar el clima")).color(c.text_muted)).frame_when_inactive(false))
                    .clicked()
                {
                    never = true;
                }
            });
        });
    });
    if go {
        search(app);
    }
    if resp.should_close() {
        close = true;
    }
    if let Some(p) = chosen {
        save(app, |w| {
            w.place = Some(p);
            w.enabled = true;
            w.dismissed = false;
        });
        close = true;
    }
    if never {
        save(app, |w| {
            w.enabled = false;
            w.dismissed = true;
        });
        close = true;
    }
    if close {
        app.today.weather.setup_open = false;
    }
}

/// Nube: tres círculos sobre una base redondeada.
fn cloud(p: &Painter, r: Rect, fill: Color32) {
    let (w, h) = (r.width(), r.height());
    let at = |x: f32, y: f32| pos2(r.left() + w * x, r.top() + h * y);
    p.circle_filled(at(0.36, 0.56), w * 0.19, fill);
    p.circle_filled(at(0.58, 0.46), w * 0.24, fill);
    p.circle_filled(at(0.76, 0.6), w * 0.16, fill);
    p.rect_filled(Rect::from_min_max(at(0.18, 0.56), at(0.9, 0.76)), w * 0.1, fill);
}

/// Icono del cielo dibujado en `r`.
pub fn paint_sky(p: &Painter, r: Rect, sky: Sky, night: bool, ink: Color32, sun: Color32, water: Color32) {
    let (w, h) = (r.width(), r.height());
    let at = |x: f32, y: f32| pos2(r.left() + w * x, r.top() + h * y);
    let sun_or_moon = |p: &Painter, rr: Rect| {
        if night {
            p.circle_filled(rr.center(), rr.width() * 0.32, ink.gamma_multiply(0.8));
        } else {
            icons::paint(p, rr, Icon::Sun, sun, sun);
        }
    };
    let low = Rect::from_min_max(at(0.0, 0.12), at(1.0, 0.95));
    match sky {
        Sky::Clear => sun_or_moon(p, r),
        Sky::PartlyCloudy => {
            sun_or_moon(p, Rect::from_min_max(at(0.0, 0.0), at(0.62, 0.62)));
            cloud(p, low, ink);
        }
        Sky::Cloudy => cloud(p, r, ink),
        Sky::Fog => {
            for (i, y) in [0.35, 0.55, 0.75].iter().enumerate() {
                let x0 = if i == 1 { 0.1 } else { 0.2 };
                p.line_segment([at(x0, *y), at(0.9 - x0 * 0.5, *y)], Stroke::new(2.0, ink));
            }
        }
        Sky::Rain | Sky::Showers => {
            if sky == Sky::Showers {
                sun_or_moon(p, Rect::from_min_max(at(0.0, 0.0), at(0.55, 0.55)));
            }
            cloud(p, Rect::from_min_max(at(0.0, -0.1), at(1.0, 0.75)), ink);
            for x in [0.35, 0.55, 0.75] {
                p.line_segment([at(x, 0.72), at(x - 0.08, 0.95)], Stroke::new(2.0, water));
            }
        }
        Sky::Snow | Sky::Sleet => {
            cloud(p, Rect::from_min_max(at(0.0, -0.1), at(1.0, 0.75)), ink);
            for (x, y) in [(0.35, 0.82), (0.55, 0.92), (0.75, 0.82)] {
                p.circle_filled(at(x, y), w * 0.05, if sky == Sky::Sleet { water } else { ink });
            }
        }
        Sky::Thunder => {
            cloud(p, Rect::from_min_max(at(0.0, -0.1), at(1.0, 0.75)), ink);
            icons::paint(p, Rect::from_min_max(at(0.38, 0.55), at(0.72, 1.0)), Icon::Bolt, sun, sun);
        }
    }
}

impl DeskApp {
    /// Capturas: el clima de Ciudad de México con datos fijos (sin red).
    pub fn demo_weather(&mut self) {
        let place = Place {
            name: "Ciudad de México".into(),
            admin: "Ciudad de México".into(),
            country: "México".into(),
            lat: 19.4285,
            lon: -99.1277,
        };
        self.session.prefs.weather.enabled = true;
        self.session.prefs.weather.place = Some(place.clone());
        let w = &mut self.today.weather;
        w.for_place = Some(place);
        w.last = Some(Instant::now());
        w.data = Some(DayWeather {
            date: self.session.today(),
            now_c: 16.3,
            min_c: 11.0,
            max_c: 22.4,
            sky: Sky::PartlyCloudy,
            night: false,
            precip_mm: 0.0,
        });
    }

    /// Capturas: «Mi día» personalizándose.
    pub fn demo_today_edit(&mut self) {
        self.today.editing = true;
    }
}
