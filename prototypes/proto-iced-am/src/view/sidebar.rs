//! Sidebar: search field and grouped library sources.

use common::tokens::ColorTokens;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, row, stack, text, text_input};
use iced::{Border, Element, Length, Padding, border};

use super::components::{icon, single_line, spacer};
use crate::app::{App, Message};
use crate::assets::Icon;
use crate::style::{BODY, LABEL, color, filled, flat_button};

pub const SIDEBAR_WIDTH: f32 = 240.0;

/// A sidebar entry: global index (used as selection id), label and icon.
pub struct SidebarItem {
    pub label: &'static str,
    pub icon: Icon,
}

const LIBRARY_ITEMS: [SidebarItem; 4] = [
    SidebarItem {
        label: "Ajouts récents",
        icon: Icon::ClockCounterClockwise,
    },
    SidebarItem {
        label: "Artistes",
        icon: Icon::MicrophoneStage,
    },
    SidebarItem {
        label: "Albums",
        icon: Icon::VinylRecord,
    },
    SidebarItem {
        label: "Morceaux",
        icon: Icon::MusicNote,
    },
];
const ANALYSIS_ITEMS: [SidebarItem; 3] = [
    SidebarItem {
        label: "Authenticité",
        icon: Icon::SealCheck,
    },
    SidebarItem {
        label: "Spectrogrammes",
        icon: Icon::Waveform,
    },
    SidebarItem {
        label: "Sortie audio",
        icon: Icon::SpeakerHifi,
    },
];
const PLAYLIST_ITEMS: [SidebarItem; 3] = [
    SidebarItem {
        label: "Écoute critique",
        icon: Icon::Playlist,
    },
    SidebarItem {
        label: "Hi-Res 24/192",
        icon: Icon::Playlist,
    },
    SidebarItem {
        label: "Vinyles numérisés",
        icon: Icon::Playlist,
    },
];

/// Label and icon of the sidebar item `item_index` (used by the placeholder view).
pub fn sidebar_item(item_index: usize) -> &'static SidebarItem {
    let library_count = LIBRARY_ITEMS.len();
    let analysis_end = library_count + ANALYSIS_ITEMS.len();
    if item_index < library_count {
        &LIBRARY_ITEMS[item_index]
    } else if item_index < analysis_end {
        &ANALYSIS_ITEMS[item_index - library_count]
    } else {
        &PLAYLIST_ITEMS[item_index - analysis_end]
    }
}

fn section<'a>(
    app: &App,
    tokens: &'static ColorTokens,
    title: &'static str,
    items: &'static [SidebarItem],
    first_index: usize,
) -> Element<'a, Message> {
    let header = container(single_line(
        title,
        LABEL,
        color(tokens.text_secondary),
        Length::Fill,
        Horizontal::Left,
    ))
    .padding(Padding {
        left: 20.0,
        ..Padding::ZERO
    })
    .height(24)
    .align_y(Vertical::Center);
    let mut entries = column![header];
    for (offset, item) in items.iter().enumerate() {
        entries = entries.push(item_button(app, tokens, item, first_index + offset));
    }
    entries.into()
}

fn item_button<'a>(
    app: &App,
    tokens: &'static ColorTokens,
    item: &'static SidebarItem,
    item_index: usize,
) -> Element<'a, Message> {
    let is_selected = app.selected_sidebar_item == item_index;
    let selected_fill = tokens.fill_sidebar_selected;
    let hover_fill = tokens.fill_sidebar_hover;
    let hover_style = flat_button(None, hover_fill, hover_fill, 6.0);
    let content = container(
        row![
            icon(app, item.icon, 16.0, color(tokens.accent)),
            single_line(
                item.label,
                BODY,
                color(tokens.text_primary),
                Length::Fill,
                Horizontal::Left
            ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    )
    .padding(Padding {
        left: 10.0,
        right: 10.0,
        ..Padding::ZERO
    })
    .height(28)
    .align_y(Vertical::Center);
    container(
        button(content)
            .padding(0)
            .width(220)
            .height(28)
            .style(move |theme, status| {
                if is_selected {
                    flat_button(Some(selected_fill), selected_fill, selected_fill, 6.0)(
                        theme, status,
                    )
                } else {
                    hover_style(theme, status)
                }
            })
            .on_press(Message::SidebarSelected(item_index)),
    )
    .padding(Padding {
        left: 10.0,
        ..Padding::ZERO
    })
    .into()
}

fn search_field<'a>(app: &'a App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let input = text_input("Rechercher", &app.query)
        .on_input(Message::SearchChanged)
        .font(crate::assets::INTER)
        .size(13)
        .line_height(text::LineHeight::Absolute(18.into()))
        .padding(Padding {
            top: 5.0,
            right: 8.0,
            bottom: 5.0,
            left: 30.0,
        })
        .width(216)
        .style(move |_, status| {
            let is_focused = matches!(status, text_input::Status::Focused { .. });
            let mut selection = color(tokens.accent);
            selection.a = 0.35;
            text_input::Style {
                background: color(tokens.surface_field).into(),
                border: Border {
                    color: color(tokens.focus_ring),
                    width: if is_focused { 2.0 } else { 0.0 },
                    radius: border::radius(7),
                },
                icon: color(tokens.text_secondary),
                placeholder: color(tokens.text_secondary),
                value: color(tokens.text_primary),
                selection,
            }
        });
    // The icon layer carries no widget that captures clicks, so presses reach the input.
    let icon_layer = container(icon(
        app,
        Icon::MagnifyingGlass,
        14.0,
        color(tokens.text_secondary),
    ))
    .padding(Padding {
        top: 7.0,
        left: 9.0,
        ..Padding::ZERO
    });
    container(stack![input, icon_layer].width(216).height(28))
        .padding(Padding {
            top: 16.0,
            left: 12.0,
            ..Padding::ZERO
        })
        .into()
}

pub fn sidebar<'a>(app: &'a App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let library_count = LIBRARY_ITEMS.len();
    let analysis_count = ANALYSIS_ITEMS.len();
    let content = column![
        search_field(app, tokens),
        spacer(1, 16),
        section(app, tokens, "Bibliothèque", &LIBRARY_ITEMS, 0),
        spacer(1, 16),
        section(app, tokens, "Analyse", &ANALYSIS_ITEMS, library_count),
        spacer(1, 16),
        section(
            app,
            tokens,
            "Playlists",
            &PLAYLIST_ITEMS,
            library_count + analysis_count
        ),
    ]
    .width(Length::Fill);
    let edge = container(spacer(1, Length::Fill)).style(filled(tokens.separator_chrome, 0.0));
    container(row![content, edge])
        .width(SIDEBAR_WIDTH)
        .height(Length::Fill)
        .style(filled(tokens.surface_sidebar, 0.0))
        .into()
}
