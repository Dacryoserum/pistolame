//! Albums grid: virtualized rows of artwork tiles.

use common::tokens::ColorTokens;
use iced::alignment::Horizontal;
use iced::widget::{button, column, container, hover, image, row, scrollable, stack};
use iced::{ContentFit, Element, Length, Padding};

use super::components::{icon, single_line, spacer};
use super::page_title;
use crate::app::{App, Message, grid_scroll_id};
use crate::assets::Icon;
use crate::style::{
    BODY, BODY_MEDIUM, artwork_style, color, filled, flat_button, invisible_button, scrollbar_style,
};

const GRID_GUTTER: f32 = 32.0;
const GRID_GAP: f32 = 24.0;
const TILE_TEXT_HEIGHT: f32 = 40.0;
const TITLE_BLOCK_HEIGHT: f32 = 76.0;
const TOOLBAR_HEIGHT: f32 = 60.0;
/// Smallest tile (160) plus the gap: the column count formula divides by this.
const COLUMN_PITCH_DIVISOR: f32 = 160.0 + GRID_GAP;
const MIN_COLUMNS: f32 = 3.0;
const MAX_COLUMNS: f32 = 8.0;
const NO_RESULTS_TOP: f32 = 96.0;
/// Rows kept alive above and below the visible ones.
const EXTRA_ROWS: usize = 1;

/// Tile layout derived from the content width (section 9 of the plan).
pub struct GridGeometry {
    pub columns: usize,
    pub tile: f32,
    pub pitch: f32,
}

pub fn grid_geometry(window_width: f32) -> GridGeometry {
    let inner_width = window_width - 240.0 - 2.0 * GRID_GUTTER;
    let columns = ((inner_width + GRID_GAP) / COLUMN_PITCH_DIVISOR)
        .floor()
        .clamp(MIN_COLUMNS, MAX_COLUMNS);
    let tile = ((inner_width - GRID_GAP * (columns - 1.0)) / columns).floor();
    GridGeometry {
        columns: columns as usize,
        tile,
        pitch: tile + TILE_TEXT_HEIGHT + GRID_GAP,
    }
}

fn play_overlay<'a>(
    app: &App,
    tokens: &'static ColorTokens,
    album_index: usize,
    tile: f32,
) -> Element<'a, Message> {
    let veil = container(spacer(tile, tile)).style(filled(tokens.overlay_artwork_hover, 0.0));
    let play_button = button(
        container(icon(app, Icon::PlayFill, 14.0, color(tokens.overlay_icon)))
            .padding(Padding {
                left: 2.0,
                ..Padding::ZERO
            })
            .center(Length::Fill),
    )
    .padding(0)
    .width(32)
    .height(32)
    .style(flat_button(
        Some(tokens.overlay_button),
        tokens.overlay_button_hover,
        tokens.overlay_button_hover,
        16.0,
    ))
    .on_press(Message::AlbumPlayRequested(album_index));
    stack![
        veil,
        container(play_button).padding(Padding {
            top: tile - 40.0,
            left: 8.0,
            ..Padding::ZERO
        })
    ]
    .into()
}

fn album_tile<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    album_index: usize,
    tile: f32,
) -> Element<'a, Message> {
    let album = &app.albums[album_index];
    let artwork = container(
        image(app.artworks[album.artwork_variant].clone())
            .width(tile)
            .height(tile)
            .content_fit(ContentFit::Fill),
    )
    .style(artwork_style(tokens));
    button(column![
        hover(artwork, play_overlay(app, tokens, album_index, tile)),
        spacer(1, 8),
        single_line(
            album.title.clone(),
            BODY_MEDIUM,
            color(tokens.text_primary),
            Length::Fill,
            Horizontal::Left,
        ),
        single_line(
            album.artist.clone(),
            BODY,
            color(tokens.text_secondary),
            Length::Fill,
            Horizontal::Left,
        ),
    ])
    .padding(0)
    .width(tile)
    .style(invisible_button())
    .on_press(Message::AlbumOpened(album_index))
    .into()
}

fn no_results<'a>(app: &'a App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let message = common::catalog::no_results_message(&app.query);
    container(
        column![
            icon(
                app,
                Icon::MagnifyingGlass,
                32.0,
                color(tokens.text_tertiary)
            ),
            spacer(1, 12),
            single_line(
                message,
                BODY,
                color(tokens.text_secondary),
                Length::Shrink,
                Horizontal::Center
            ),
        ]
        .align_x(Horizontal::Center),
    )
    .padding(Padding {
        top: NO_RESULTS_TOP,
        ..Padding::ZERO
    })
    .width(Length::Fill)
    .align_x(Horizontal::Center)
    .into()
}

fn virtual_rows<'a>(app: &'a App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let geometry = grid_geometry(app.window_size.width);
    let viewport_height = (app.window_size.height - TOOLBAR_HEIGHT - TITLE_BLOCK_HEIGHT).max(0.0);
    let row_count = app.visible.len().div_ceil(geometry.columns);
    let total_height = (row_count as f32 * geometry.pitch - GRID_GAP + GRID_GUTTER).max(0.0);

    let first_visible_row = (app.grid_scroll_offset / geometry.pitch).floor() as usize;
    let first_row = first_visible_row.saturating_sub(EXTRA_ROWS).min(row_count);
    let built_row_count = (viewport_height / geometry.pitch).ceil() as usize + 1 + 2 * EXTRA_ROWS;
    let end_row = (first_row + built_row_count).min(row_count);

    let top_space = first_row as f32 * geometry.pitch;
    let bottom_space = (total_height - end_row as f32 * geometry.pitch).max(0.0);
    let mut rows = column![spacer(1, top_space)];
    for row_index in first_row..end_row {
        let first_tile = row_index * geometry.columns;
        let last_tile = (first_tile + geometry.columns).min(app.visible.len());
        let tiles = app.visible[first_tile..last_tile]
            .iter()
            .map(|album_index| album_tile(app, tokens, *album_index, geometry.tile));
        rows = rows.push(
            container(row(tiles).spacing(GRID_GAP))
                .padding(Padding {
                    left: GRID_GUTTER,
                    ..Padding::ZERO
                })
                .height(geometry.pitch),
        );
    }
    rows.push(spacer(1, bottom_space))
        .width(Length::Fill)
        .into()
}

pub fn albums_grid<'a>(app: &'a App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let body: Element<'a, Message> = if app.visible.is_empty() {
        no_results(app, tokens)
    } else {
        scrollable(virtual_rows(app, tokens))
            .id(grid_scroll_id())
            .on_scroll(|viewport| Message::GridScrolled(viewport.absolute_offset().y))
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(6)
                    .scroller_width(6)
                    .margin(3),
            ))
            .style(scrollbar_style(tokens))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };
    column![page_title(tokens, "Albums"), spacer(1, 20), body]
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Left)
        .into()
}
