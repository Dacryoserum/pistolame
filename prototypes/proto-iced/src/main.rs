#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::Instant;

use common::color as c;
use iced::widget::{button, column, container, mouse_area, row, scrollable, space, stack, text, Space};
use iced::{
    border, font, gradient, window, Background, Border, Color, Element, Font, Length, Radians,
    Subscription, Theme,
};

const ROW_H: f32 = 36.0;

fn rgb(x: u32) -> Color {
    Color::from_rgb8((x >> 16) as u8, (x >> 8) as u8, x as u8)
}

fn bold() -> Font {
    Font { weight: font::Weight::Bold, ..Font::DEFAULT }
}

fn fill(bg: u32, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(rgb(bg))),
        border: border::rounded(radius),
        ..Default::default()
    }
}

fn cover<'a>(c1: u32, c2: u32, size: f32, radius: f32) -> Element<'a, Message> {
    let g = gradient::Linear::new(Radians(std::f32::consts::FRAC_PI_4 * 3.0))
        .add_stop(0.0, rgb(c1))
        .add_stop(1.0, rgb(c2));
    container(Space::new())
        .width(size)
        .height(size)
        .style(move |_| container::Style {
            background: Some(Background::Gradient(g.into())),
            border: border::rounded(radius),
            ..Default::default()
        })
        .into()
}

#[derive(Debug, Clone)]
enum Message {
    Frame(Instant),
    TogglePlay,
    Select(usize),
    Scrolled(f32, f32),
}

struct App {
    albums: Vec<common::Album>,
    tracks: Vec<common::Track>,
    started: Instant,
    last: Instant,
    selected: usize,
    playing: bool,
    elapsed: f32,
    spectrum: [f32; common::SPECTRUM_BARS],
    scroll_y: f32,
    viewport_h: f32,
}

impl App {
    fn new() -> Self {
        let albums = common::albums();
        let tracks = common::tracks(&albums);
        Self {
            albums,
            tracks,
            started: Instant::now(),
            last: Instant::now(),
            selected: 0,
            playing: true,
            elapsed: 0.0,
            spectrum: [0.0; common::SPECTRUM_BARS],
            scroll_y: 0.0,
            viewport_h: 800.0,
        }
    }

    fn update(&mut self, msg: Message) {
        match msg {
            Message::Frame(now) => {
                if self.playing {
                    self.elapsed += (now - self.last).as_secs_f32();
                    self.spectrum = common::spectrum((now - self.started).as_secs_f32());
                }
                self.last = now;
                if self.elapsed >= self.tracks[self.selected].duration_s as f32 {
                    self.elapsed = 0.0;
                }
            }
            Message::TogglePlay => {
                self.playing = !self.playing;
                self.last = Instant::now();
            }
            Message::Select(i) => {
                self.selected = i;
                self.elapsed = 0.0;
                self.playing = true;
            }
            Message::Scrolled(y, h) => {
                self.scroll_y = y;
                self.viewport_h = h;
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        window::frames().map(Message::Frame)
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let nav = |label: &'static str, active: bool| {
            let bar = container(Space::new()).width(3).height(14).style(fill(
                if active { c::ACCENT } else { c::SURFACE },
                2.0,
            ));
            let label = text(label)
                .size(13)
                .color(rgb(if active { c::TEXT } else { c::MUTED }))
                .font(if active { bold() } else { Font::DEFAULT });
            container(row![bar, label].spacing(10).align_y(iced::Alignment::Center))
                .padding([0, 12])
                .height(34)
                .width(Length::Fill)
                .align_y(iced::Alignment::Center)
                .style(fill(if active { c::SURFACE_2 } else { c::SURFACE }, 8.0))
        };
        let output = container(
            column![
                text("SORTIE").size(10).color(rgb(c::MUTED)),
                row![
                    container(Space::new()).width(8).height(8).style(fill(c::OK, 4.0)),
                    text("FiiO K11 · WASAPI excl.").size(13).color(rgb(c::TEXT)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(4),
        )
        .padding(12)
        .width(Length::Fill)
        .style(fill(c::SURFACE_2, 10.0));

        container(
            column![
                container(text("PISTOLAME").size(15).font(bold()).color(rgb(c::ACCENT)))
                    .height(48)
                    .align_y(iced::Alignment::Center),
                nav("Bibliothèque", true),
                nav("Albums", false),
                nav("Artistes", false),
                nav("Dossiers", false),
                nav("Analyse", false),
                space().height(Length::Fill),
                output,
            ]
            .spacing(4),
        )
        .padding(16)
        .width(220)
        .height(Length::Fill)
        .style(fill(c::SURFACE, 0.0))
        .into()
    }

    fn track_row(&self, i: usize) -> Element<'_, Message> {
        let t = &self.tracks[i];
        let sel = i == self.selected;
        let muted = rgb(c::MUTED);
        let r = row![
            text(t.number.to_string()).size(13).width(32).color(muted),
            text(&t.title).size(13).width(Length::FillPortion(20)).color(rgb(if sel { c::ACCENT } else { c::TEXT })),
            text(&t.artist).size(13).width(Length::FillPortion(14)).color(muted),
            text(&t.album).size(13).width(Length::FillPortion(14)).color(muted),
            text(t.format).width(96).size(11).color(muted),
            text(common::fmt_time(t.duration_s)).size(13).width(48).color(muted).align_x(iced::Alignment::End),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center);
        mouse_area(
            container(r)
                .padding([0, 12])
                .height(ROW_H)
                .width(Length::Fill)
                .align_y(iced::Alignment::Center)
                .style(fill(if sel { c::SURFACE_2 } else { c::BG }, 6.0)),
        )
        .on_press(Message::Select(i))
        .into()
    }

    fn main_panel(&self) -> Element<'_, Message> {
        let header = row![
            text("Bibliothèque").size(26).font(bold()).color(rgb(c::TEXT)),
            space().width(Length::Fill),
            text(format!("{} titres · {} albums", common::TRACK_COUNT, common::ALBUM_COUNT))
                .color(rgb(c::MUTED)),
        ]
        .align_y(iced::Alignment::Center)
        .height(40);

        let strip = scrollable(
            row(self.albums.iter().take(40).map(|a| {
                column![
                    cover(a.cover.0, a.cover.1, 128.0, 10.0),
                    text(&a.title).size(13).color(rgb(c::TEXT)),
                    text(&a.artist).size(11).color(rgb(c::MUTED)),
                ]
                .spacing(4)
                .width(128)
                .into()
            }))
            .spacing(14),
        )
        .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new()))
        .height(180);

        let head = |s: &'static str| text(s).size(11).color(rgb(c::MUTED));
        let table_head = container(
            row![
                head("#").width(32),
                head("TITRE").width(Length::FillPortion(20)),
                head("ARTISTE").width(Length::FillPortion(14)),
                head("ALBUM").width(Length::FillPortion(14)),
                head("FORMAT").width(96),
                head("DURÉE").width(48).align_x(iced::Alignment::End),
            ]
            .spacing(12),
        )
        .padding([0, 12])
        .height(28)
        .align_y(iced::Alignment::Center);

        // iced n'a pas de liste virtualisée : on ne construit que les lignes visibles.
        let n = self.tracks.len();
        let first = ((self.scroll_y / ROW_H) as usize).min(n);
        let last = (first + (self.viewport_h / ROW_H) as usize + 2).min(n);
        let rows = column(
            std::iter::once(Space::new().height(first as f32 * ROW_H).into())
                .chain((first..last).map(|i| self.track_row(i)))
                .chain(std::iter::once(Space::new().height((n - last) as f32 * ROW_H).into())),
        );
        let list = scrollable(rows)
            .on_scroll(|v| Message::Scrolled(v.absolute_offset().y, v.bounds().height))
            .height(Length::Fill);

        container(column![header, strip, table_head, list].spacing(16))
            .padding(24)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn now_playing(&self) -> Element<'_, Message> {
        let t = &self.tracks[self.selected];
        let a = &self.albums[t.album_index];
        let badge = container(text("✓ BIT-PERFECT · 24/96 → 24/96").size(11).font(bold()).color(rgb(c::OK)))
            .padding([4, 10])
            .style(|_| container::Style {
                background: Some(Background::Color(Color { a: 0.14, ..rgb(c::OK) })),
                border: Border { color: Color { a: 0.4, ..rgb(c::OK) }, width: 1.0, radius: 12.0.into() },
                ..Default::default()
            });
        let signal = column(common::SIGNAL_PATH.iter().map(|(l, v)| {
            row![
                container(Space::new()).width(6).height(6).style(fill(c::OK, 3.0)),
                text(*l).size(11).width(70).color(rgb(c::MUTED)),
                text(*v).size(12).color(rgb(c::TEXT)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        }))
        .spacing(6);

        let bars = row(self.spectrum.iter().map(|v| {
            let g = gradient::Linear::new(Radians(0.0))
                .add_stop(0.0, rgb(c::ACCENT))
                .add_stop(1.0, rgb(c::OK));
            container(Space::new())
                .width(Length::Fill)
                .height((v * 98.0).max(2.0))
                .style(move |_| container::Style {
                    background: Some(Background::Gradient(g.into())),
                    border: border::rounded(1.0),
                    ..Default::default()
                })
                .into()
        }))
        .spacing(2)
        .align_y(iced::Alignment::End);
        let spectrum = container(bars)
            .padding(6)
            .height(110)
            .width(Length::Fill)
            .align_y(iced::Alignment::End)
            .style(fill(c::BG, 8.0));

        container(
            column![
                text("EN LECTURE").size(10).color(rgb(c::MUTED)),
                cover(a.cover.0, a.cover.1, 272.0, 14.0),
                column![
                    text(&t.title).size(18).font(bold()).color(rgb(c::TEXT)),
                    text(format!("{} — {}", t.artist, t.album)).size(13).color(rgb(c::MUTED)),
                ]
                .spacing(2),
                badge,
                signal,
                spectrum,
            ]
            .spacing(14),
        )
        .padding(24)
        .width(320)
        .height(Length::Fill)
        .style(fill(c::SURFACE, 0.0))
        .into()
    }

    fn player_bar(&self) -> Element<'_, Message> {
        let t = &self.tracks[self.selected];
        let round = |glyph: &'static str, primary: bool, msg: Option<Message>| {
            let size = if primary { 44.0 } else { 34.0 };
            button(text(glyph).size(if primary { 16 } else { 14 }).center())
                .width(size)
                .height(size)
                .on_press_maybe(msg)
                .style(move |_, status| button::Style {
                    background: Some(Background::Color(match (primary, status) {
                        (true, button::Status::Pressed) => rgb(0xB0832F),
                        (true, _) => rgb(c::ACCENT),
                        (false, button::Status::Hovered) => rgb(c::SURFACE_2),
                        (false, _) => Color::TRANSPARENT,
                    })),
                    text_color: rgb(if primary { c::BG } else { c::TEXT }),
                    border: border::rounded(size / 2.0),
                    ..Default::default()
                })
        };
        let total = t.duration_s as f32;
        let progress = (self.elapsed / total).clamp(0.0, 1.0);
        let track_bar = stack![
            container(Space::new()).width(Length::Fill).height(4).style(fill(c::BORDER, 2.0)),
            row![
                container(Space::new())
                    .width(Length::FillPortion((progress * 1000.0) as u16 + 1))
                    .height(4)
                    .style(fill(c::ACCENT, 2.0)),
                space().width(Length::FillPortion(((1.0 - progress) * 1000.0) as u16 + 1)),
            ],
        ];
        let middle = column![
            row![
                text(format!("{} · {}", t.title, t.artist)).size(13).color(rgb(c::TEXT)),
                space().width(Length::Fill),
                text(format!("{} / {}", common::fmt_time(self.elapsed as u32), common::fmt_time(t.duration_s)))
                    .size(12)
                    .color(rgb(c::MUTED)),
            ],
            track_bar,
        ]
        .spacing(8)
        .width(Length::Fill);
        let chip = container(text(t.format).size(11).font(bold()).color(rgb(c::ACCENT)))
            .width(120)
            .height(26)
            .center_x(120)
            .center_y(26)
            .style(fill(c::SURFACE_2, 6.0));

        container(
            row![
                row![
                    round("⏮\u{FE0E}", false, None),
                    round(if self.playing { "⏸\u{FE0E}" } else { "▶" }, true, Some(Message::TogglePlay)),
                    round("⏭\u{FE0E}", false, None),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
                middle,
                chip,
            ]
            .spacing(16)
            .align_y(iced::Alignment::Center),
        )
        .padding([0, 20])
        .height(76)
        .width(Length::Fill)
        .align_y(iced::Alignment::Center)
        .style(|_| container::Style {
            background: Some(Background::Color(rgb(c::SURFACE))),
            border: Border { color: rgb(c::BORDER), width: 1.0, radius: 0.0.into() },
            ..Default::default()
        })
        .into()
    }

    fn view(&self) -> Element<'_, Message> {
        container(column![
            row![self.sidebar(), self.main_panel(), self.now_playing()].height(Length::Fill),
            self.player_bar(),
        ])
        .style(fill(c::BG, 0.0))
        .into()
    }
}

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Pistolame — maquette iced")
        .subscription(App::subscription)
        .theme(|_: &App| Theme::Dark)
        .window_size((1280.0, 800.0))
        .run()
}
