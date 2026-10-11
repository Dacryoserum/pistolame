#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::rc::Rc;
use std::time::Instant;

use common::color as c;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

const ROW_H: f32 = 36.0;

struct Pistolame {
    albums: Rc<Vec<common::Album>>,
    tracks: Rc<Vec<common::Track>>,
    started: Instant,
    last: Instant,
    selected: usize,
    playing: bool,
    elapsed: f32,
}

fn cover(c1: u32, c2: u32, size: f32, radius: f32) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(radius))
        .bg(linear_gradient(135., linear_color_stop(rgb(c1), 0.), linear_color_stop(rgb(c2), 1.)))
}

fn label(s: impl Into<SharedString>, size: f32, color: u32) -> Div {
    div().text_size(px(size)).text_color(rgb(color)).child(s.into())
}

impl Pistolame {
    fn sidebar(&self) -> impl IntoElement {
        let nav = |id: &'static str, active: bool| {
            div()
                .id(id)
                .h(px(34.))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(8.))
                .when(active, |d| d.bg(rgb(c::SURFACE_2)))
                .hover(|s| s.bg(rgb(c::SURFACE_2)))
                .child(div().w(px(3.)).h(px(14.)).rounded(px(2.)).when(active, |d| d.bg(rgb(c::ACCENT))))
                .child(
                    label(id, 13., if active { c::TEXT } else { c::MUTED })
                        .when(active, |d| d.font_weight(FontWeight::SEMIBOLD)),
                )
        };
        div()
            .w(px(220.))
            .h_full()
            .flex_none()
            .bg(rgb(c::SURFACE))
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .h(px(48.))
                    .flex()
                    .items_center()
                    .child(label("PISTOLAME", 15., c::ACCENT).font_weight(FontWeight::EXTRA_BOLD)),
            )
            .child(nav("Bibliothèque", true))
            .child(nav("Albums", false))
            .child(nav("Artistes", false))
            .child(nav("Dossiers", false))
            .child(nav("Analyse", false))
            .child(div().flex_1())
            .child(
                div()
                    .p(px(12.))
                    .rounded(px(10.))
                    .bg(rgb(c::SURFACE_2))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(label("SORTIE", 10., c::MUTED))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().size(px(8.)).rounded_full().bg(rgb(c::OK)))
                            .child(label("FiiO K11 · WASAPI excl.", 13., c::TEXT)),
                    ),
            )
    }

    fn main_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let strip = div()
            .id("albums")
            .h(px(176.))
            .flex_none()
            .overflow_x_scroll()
            .flex()
            .gap(px(14.))
            .children(self.albums.iter().take(40).map(|a| {
                div()
                    .w(px(128.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(cover(a.cover.0, a.cover.1, 128., 10.))
                    .child(label(a.title.clone(), 13., c::TEXT).truncate())
                    .child(label(a.artist.clone(), 11., c::MUTED).truncate())
            }));

        let head = |s: &'static str| label(s, 11., c::MUTED);
        let columns = |r: Div, n: Div, t: Div, ar: Div, al: Div, f: Div, d: Div| {
            r.w_full()
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(12.))
                .child(n.w(px(32.)).flex_none())
                .child(t.flex_grow(0.4).flex_basis(px(0.)).min_w(px(0.)).truncate())
                .child(ar.flex_grow(0.28).flex_basis(px(0.)).min_w(px(0.)).truncate())
                .child(al.flex_grow(0.28).flex_basis(px(0.)).min_w(px(0.)).truncate())
                .child(f.w(px(96.)).flex_none())
                .child(d.w(px(48.)).flex_none().flex().justify_end())
        };

        let tracks = self.tracks.clone();
        let selected = self.selected;
        let list = uniform_list(
            "tracks",
            tracks.len(),
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|i| {
                        let t = &tracks[i];
                        let sel = i == selected;
                        columns(
                            div(),
                            label(t.number.to_string(), 13., c::MUTED),
                            label(t.title.clone(), 13., if sel { c::ACCENT } else { c::TEXT }),
                            label(t.artist.clone(), 13., c::MUTED),
                            label(t.album.clone(), 13., c::MUTED),
                            label(t.format, 11., c::MUTED),
                            label(common::fmt_time(t.duration_s), 13., c::MUTED),
                        )
                        .id(i)
                        .h(px(ROW_H))
                        .rounded(px(6.))
                        .when(sel, |d| d.bg(rgb(c::SURFACE_2)))
                        .hover(|s| s.bg(rgba(0xffffff08)))
                        .on_click(cx.listener(move |this: &mut Self, _, _, cx| {
                            this.selected = i;
                            this.elapsed = 0.;
                            this.playing = true;
                            cx.notify();
                        }))
                    })
                    .collect()
            }),
        )
        .flex_1();

        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label("Bibliothèque", 26., c::TEXT).font_weight(FontWeight::BOLD))
                    .child(label(
                        format!("{} titres · {} albums", common::TRACK_COUNT, common::ALBUM_COUNT),
                        13.,
                        c::MUTED,
                    )),
            )
            .child(strip)
            .child(
                columns(
                    div(),
                    head("#"),
                    head("TITRE"),
                    head("ARTISTE"),
                    head("ALBUM"),
                    head("FORMAT"),
                    head("DURÉE"),
                )
                .h(px(28.)),
            )
            .child(list)
    }

    fn now_playing(&self) -> impl IntoElement {
        let t = &self.tracks[self.selected];
        let a = &self.albums[t.album_index];
        let spectrum = if self.playing {
            common::spectrum(self.started.elapsed().as_secs_f32())
        } else {
            common::spectrum(0.0)
        };
        div()
            .w(px(320.))
            .h_full()
            .flex_none()
            .bg(rgb(c::SURFACE))
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(label("EN LECTURE", 10., c::MUTED))
            .child(cover(a.cover.0, a.cover.1, 272., 14.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(label(t.title.clone(), 18., c::TEXT).font_weight(FontWeight::BOLD).truncate())
                    .child(label(format!("{} — {}", t.artist, t.album), 13., c::MUTED).truncate()),
            )
            .child(
                div().flex().child(
                    div()
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(12.))
                        .bg(rgba((c::OK << 8) | 0x24))
                        .border_1()
                        .border_color(rgba((c::OK << 8) | 0x66))
                        .child(
                            label("✓ BIT-PERFECT · 24/96 → 24/96", 11., c::OK).font_weight(FontWeight::BOLD),
                        ),
                ),
            )
            .child(div().flex().flex_col().gap(px(6.)).children(common::SIGNAL_PATH.iter().map(|(l, v)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().size(px(6.)).rounded_full().bg(rgb(c::OK)))
                    .child(label(*l, 11., c::MUTED).w(px(70.)))
                    .child(label(*v, 12., c::TEXT))
            })))
            .child(
                div()
                    .h(px(110.))
                    .p(px(6.))
                    .rounded(px(8.))
                    .bg(rgb(c::BG))
                    .flex()
                    .items_end()
                    .gap(px(2.))
                    .children(spectrum.iter().map(|v| {
                        div()
                            .flex_1()
                            .h(px((v * 98.).max(2.)))
                            .rounded(px(1.))
                            .bg(linear_gradient(
                                0.,
                                linear_color_stop(rgb(c::OK), 1.),
                                linear_color_stop(rgb(c::ACCENT), 0.),
                            ))
                    })),
            )
    }

    fn player_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = &self.tracks[self.selected];
        let progress = (self.elapsed / t.duration_s as f32).clamp(0., 1.);
        let round = |id: &'static str, glyph: &'static str, primary: bool| {
            let size = if primary { 44. } else { 34. };
            div()
                .id(id)
                .size(px(size))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(if primary { 16. } else { 14. }))
                .text_color(rgb(if primary { c::BG } else { c::TEXT }))
                .when(primary, |d| d.bg(rgb(c::ACCENT)).active(|s| s.bg(rgb(0xB0832F))))
                .when(!primary, |d| d.hover(|s| s.bg(rgb(c::SURFACE_2))))
                .child(glyph)
        };
        div()
            .h(px(76.))
            .flex_none()
            .px(px(20.))
            .bg(rgb(c::SURFACE))
            .border_t_1()
            .border_color(rgb(c::BORDER))
            .flex()
            .items_center()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(round("prev", "⏮\u{FE0E}", false))
                    .child(
                        round("play", if self.playing { "⏸\u{FE0E}" } else { "▶" }, true).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.playing = !this.playing;
                                this.last = Instant::now();
                                cx.notify();
                            },
                        )),
                    )
                    .child(round("next", "⏭\u{FE0E}", false)),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(label(format!("{} · {}", t.title, t.artist), 13., c::TEXT))
                            .child(label(
                                format!(
                                    "{} / {}",
                                    common::fmt_time(self.elapsed as u32),
                                    common::fmt_time(t.duration_s)
                                ),
                                12.,
                                c::MUTED,
                            )),
                    )
                    .child(
                        div()
                            .h(px(4.))
                            .w_full()
                            .rounded(px(2.))
                            .bg(rgb(c::BORDER))
                            .child(div().h_full().w(relative(progress)).rounded(px(2.)).bg(rgb(c::ACCENT))),
                    ),
            )
            .child(
                div()
                    .w(px(120.))
                    .h(px(26.))
                    .rounded(px(6.))
                    .bg(rgb(c::SURFACE_2))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(label(t.format, 11., c::ACCENT).font_weight(FontWeight::BOLD)),
            )
    }
}

impl Render for Pistolame {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Boucle d'animation : on avance l'horloge et on redemande une frame tant que ça joue.
        let now = Instant::now();
        if self.playing {
            self.elapsed += (now - self.last).as_secs_f32();
            if self.elapsed >= self.tracks[self.selected].duration_s as f32 {
                self.elapsed = 0.;
            }
            window.request_animation_frame();
        }
        self.last = now;

        div()
            .size_full()
            .bg(rgb(c::BG))
            .text_color(rgb(c::TEXT))
            .text_size(px(13.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.sidebar())
                    .child(self.main_panel(cx))
                    .child(self.now_playing()),
            )
            .child(self.player_bar(cx))
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let albums = common::albums();
        let tracks = common::tracks(&albums);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("Pistolame — maquette GPUI".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |_, cx| {
            cx.new(|_| Pistolame {
                albums: Rc::new(albums),
                tracks: Rc::new(tracks),
                started: Instant::now(),
                last: Instant::now(),
                selected: 0,
                playing: true,
                elapsed: 0.,
            })
        })
        .expect("ouverture de la fenêtre");
        cx.activate(true);
    });
}
