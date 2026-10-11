//! Pushes the `common` color tables into the Slint `Tokens` global.

use crate::{AppWindow, Tokens};
use common::tokens::{ColorTokens, split_rgba};
use slint::{Color, ComponentHandle};

fn slint_color(token: u32) -> Color {
    let [red, green, blue, alpha] = split_rgba(token);
    Color::from_argb_u8(alpha, red, green, blue)
}

/// Applies every token of `table` at once, so the whole UI changes theme in one frame.
pub fn apply_tokens(window: &AppWindow, table: &ColorTokens) {
    let tokens = window.global::<Tokens>();
    tokens.set_surface_content(slint_color(table.surface_content));
    tokens.set_surface_sidebar(slint_color(table.surface_sidebar));
    tokens.set_surface_toolbar(slint_color(table.surface_toolbar));
    tokens.set_surface_lcd(slint_color(table.surface_lcd));
    tokens.set_surface_raised(slint_color(table.surface_raised));
    tokens.set_surface_field(slint_color(table.surface_field));
    tokens.set_border_lcd(slint_color(table.border_lcd));
    tokens.set_border_raised(slint_color(table.border_raised));
    tokens.set_separator(slint_color(table.separator));
    tokens.set_separator_chrome(slint_color(table.separator_chrome));
    tokens.set_fill_control(slint_color(table.fill_control));
    tokens.set_fill_control_hover(slint_color(table.fill_control_hover));
    tokens.set_fill_icon_hover(slint_color(table.fill_icon_hover));
    tokens.set_fill_icon_pressed(slint_color(table.fill_icon_pressed));
    tokens.set_fill_row_hover(slint_color(table.fill_row_hover));
    tokens.set_fill_row_pressed(slint_color(table.fill_row_pressed));
    tokens.set_fill_row_zebra(slint_color(table.fill_row_zebra));
    tokens.set_fill_sidebar_hover(slint_color(table.fill_sidebar_hover));
    tokens.set_fill_sidebar_selected(slint_color(table.fill_sidebar_selected));
    tokens.set_rail(slint_color(table.rail));
    tokens.set_rail_fill(slint_color(table.rail_fill));
    tokens.set_scrubber_thumb(slint_color(table.scrubber_thumb));
    tokens.set_scrollbar_thumb(slint_color(table.scrollbar_thumb));
    tokens.set_text_primary(slint_color(table.text_primary));
    tokens.set_text_secondary(slint_color(table.text_secondary));
    tokens.set_text_tertiary(slint_color(table.text_tertiary));
    tokens.set_text_on_accent(slint_color(table.text_on_accent));
    tokens.set_accent(slint_color(table.accent));
    tokens.set_accent_hover(slint_color(table.accent_hover));
    tokens.set_accent_pressed(slint_color(table.accent_pressed));
    tokens.set_focus_ring(slint_color(table.focus_ring));
    tokens.set_status_ok(slint_color(table.status_ok));
    tokens.set_status_ok_tint(slint_color(table.status_ok_tint));
    tokens.set_status_caution(slint_color(table.status_caution));
    tokens.set_status_caution_tint(slint_color(table.status_caution_tint));
    tokens.set_shadow_artwork(slint_color(table.shadow_artwork));
    tokens.set_shadow_popover(slint_color(table.shadow_popover));
    tokens.set_overlay_artwork_hover(slint_color(table.overlay_artwork_hover));
    tokens.set_overlay_button(slint_color(table.overlay_button));
    tokens.set_overlay_button_hover(slint_color(table.overlay_button_hover));
    tokens.set_overlay_icon(slint_color(table.overlay_icon));
    tokens.set_spectrogram_label(slint_color(table.spectrogram_label));
    tokens.set_spectrogram_cutoff(slint_color(table.spectrogram_cutoff));
}
