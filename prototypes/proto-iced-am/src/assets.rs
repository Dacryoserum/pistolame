//! Embedded fonts, SVG icons and pre-built image handles.

use common::artwork::{ARTWORK_SIZE_PIXELS, ARTWORK_VARIANT_COUNT, artwork_rgba};
use iced::Font;
use iced::font::Weight;
use iced::widget::{image, svg};

/// Family name shared by the three text weights (name ID 16 of the TTFs).
pub const INTER_FAMILY_NAME: &str = "Inter";
pub const INTER: Font = Font::with_name(INTER_FAMILY_NAME);
pub const INTER_MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..INTER
};
pub const INTER_SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..INTER
};
pub const INTER_DISPLAY_BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::with_name("Inter Display")
};

/// Font files registered with the application.
pub const FONT_FILES: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/Inter-Regular.ttf").as_slice(),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf").as_slice(),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf").as_slice(),
    include_bytes!("../../assets/fonts/InterDisplay-Bold.ttf").as_slice(),
];

/// Every icon used by the UI.
#[derive(Clone, Copy, Debug)]
pub enum Icon {
    MagnifyingGlass,
    ClockCounterClockwise,
    MicrophoneStage,
    VinylRecord,
    MusicNote,
    SealCheck,
    Waveform,
    SpeakerHifi,
    Playlist,
    Shuffle,
    Repeat,
    SkipBackFill,
    PlayFill,
    PauseFill,
    SkipForwardFill,
    SpeakerLow,
    SpeakerHigh,
    CaretLeft,
    SealCheckFill,
    WarningFill,
    SlidersHorizontal,
}

impl Icon {
    const ALL: [Icon; 21] = [
        Icon::MagnifyingGlass,
        Icon::ClockCounterClockwise,
        Icon::MicrophoneStage,
        Icon::VinylRecord,
        Icon::MusicNote,
        Icon::SealCheck,
        Icon::Waveform,
        Icon::SpeakerHifi,
        Icon::Playlist,
        Icon::Shuffle,
        Icon::Repeat,
        Icon::SkipBackFill,
        Icon::PlayFill,
        Icon::PauseFill,
        Icon::SkipForwardFill,
        Icon::SpeakerLow,
        Icon::SpeakerHigh,
        Icon::CaretLeft,
        Icon::SealCheckFill,
        Icon::WarningFill,
        Icon::SlidersHorizontal,
    ];

    fn svg_bytes(self) -> &'static [u8] {
        match self {
            Icon::MagnifyingGlass => include_bytes!("../../assets/icons/magnifying-glass.svg"),
            Icon::ClockCounterClockwise => {
                include_bytes!("../../assets/icons/clock-counter-clockwise.svg")
            }
            Icon::MicrophoneStage => include_bytes!("../../assets/icons/microphone-stage.svg"),
            Icon::VinylRecord => include_bytes!("../../assets/icons/vinyl-record.svg"),
            Icon::MusicNote => include_bytes!("../../assets/icons/music-note.svg"),
            Icon::SealCheck => include_bytes!("../../assets/icons/seal-check.svg"),
            Icon::Waveform => include_bytes!("../../assets/icons/waveform.svg"),
            Icon::SpeakerHifi => include_bytes!("../../assets/icons/speaker-hifi.svg"),
            Icon::Playlist => include_bytes!("../../assets/icons/playlist.svg"),
            Icon::Shuffle => include_bytes!("../../assets/icons/shuffle.svg"),
            Icon::Repeat => include_bytes!("../../assets/icons/repeat.svg"),
            Icon::SkipBackFill => include_bytes!("../../assets/icons/skip-back-fill.svg"),
            Icon::PlayFill => include_bytes!("../../assets/icons/play-fill.svg"),
            Icon::PauseFill => include_bytes!("../../assets/icons/pause-fill.svg"),
            Icon::SkipForwardFill => include_bytes!("../../assets/icons/skip-forward-fill.svg"),
            Icon::SpeakerLow => include_bytes!("../../assets/icons/speaker-low.svg"),
            Icon::SpeakerHigh => include_bytes!("../../assets/icons/speaker-high.svg"),
            Icon::CaretLeft => include_bytes!("../../assets/icons/caret-left.svg"),
            Icon::SealCheckFill => include_bytes!("../../assets/icons/seal-check-fill.svg"),
            Icon::WarningFill => include_bytes!("../../assets/icons/warning-fill.svg"),
            Icon::SlidersHorizontal => {
                include_bytes!("../../assets/icons/sliders-horizontal.svg")
            }
        }
    }
}

/// SVG handles created once; cloning a handle is cheap and keeps its cache id.
pub struct Icons {
    handles: Vec<svg::Handle>,
}

impl Icons {
    pub fn load() -> Self {
        Self {
            handles: Icon::ALL
                .iter()
                .map(|icon| svg::Handle::from_memory(icon.svg_bytes()))
                .collect(),
        }
    }

    pub fn handle(&self, icon: Icon) -> svg::Handle {
        self.handles[icon as usize].clone()
    }
}

/// One image handle per artwork variant, created once (a new handle means a GPU re-upload).
pub fn load_artworks() -> Vec<image::Handle> {
    (0..ARTWORK_VARIANT_COUNT)
        .map(|variant| {
            image::Handle::from_rgba(
                ARTWORK_SIZE_PIXELS,
                ARTWORK_SIZE_PIXELS,
                artwork_rgba(variant),
            )
        })
        .collect()
}
