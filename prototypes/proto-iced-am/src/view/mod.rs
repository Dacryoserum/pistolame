//! Root layout: sidebar, toolbar, content view and popover layer.

mod album_detail;
mod albums_grid;
mod components;
mod popover;
mod sidebar;
mod toolbar;

use common::tokens::ColorTokens;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{Space, column, container, mouse_area, opaque, row, stack};
use iced::{Element, Length, Padding};

use crate::app::{App, Message, View};
use crate::style::{BODY, DISPLAY, color, filled, tokens};
use components::{icon, single_line, spacer};
use sidebar::sidebar_item;

/// Page title (role display) at (32, 24), 32 px high, followed by nothing.
fn page_title<'a>(tokens: &'static ColorTokens, title: &'static str) -> Element<'a, Message> {
    container(single_line(
        title,
        DISPLAY,
        color(tokens.text_primary),
        Length::Fill,
        Horizontal::Left,
    ))
    .padding(Padding {
        top: 24.0,
        left: 32.0,
        right: 32.0,
        bottom: 0.0,
    })
    .into()
}

fn placeholder<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    item_index: usize,
) -> Element<'a, Message> {
    const PLACEHOLDER_ICON_TOP: f32 = 172.0;
    let item = sidebar_item(item_index);
    let title_bottom = 24.0 + 32.0;
    column![
        page_title(tokens, item.label),
        spacer(1, PLACEHOLDER_ICON_TOP - title_bottom),
        container(
            column![
                icon(app, item.icon, 32.0, color(tokens.text_tertiary)),
                spacer(1, 12),
                single_line(
                    "Cette vue n'est pas incluse dans la maquette.",
                    BODY,
                    color(tokens.text_secondary),
                    Length::Shrink,
                    Horizontal::Center,
                ),
            ]
            .align_x(Horizontal::Center),
        )
        .width(Length::Fill)
        .align_x(Horizontal::Center),
    ]
    .into()
}

impl App {
    pub fn view(&self) -> Element<'_, Message> {
        let tokens = tokens(self.is_dark);
        let content: Element<'_, Message> = match self.view {
            View::Grid => albums_grid::albums_grid(self, tokens),
            View::Detail(album_index) => album_detail::album_detail(self, tokens, album_index),
            View::Placeholder(item_index) => placeholder(self, tokens, item_index),
        };
        let main_area = column![
            toolbar::toolbar(self, tokens),
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(filled(tokens.surface_content, 0.0)),
        ];
        let base = row![sidebar::sidebar(self, tokens), main_area];
        if !self.is_popover_open {
            return base.into();
        }
        let click_catcher = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(Message::PopoverClosed);
        let geometry = toolbar::toolbar_geometry(self.window_size.width);
        let popover_layer = container(opaque(popover::popover(self, tokens)))
            .padding(Padding {
                top: popover::POPOVER_TOP,
                right: self.window_size.width - geometry.lcd_right_in_window,
                ..Padding::ZERO
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Top);
        stack![base, click_catcher, popover_layer].into()
    }
}
