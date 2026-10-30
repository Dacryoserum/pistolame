//! The current-track page: file facts and authenticity analysis.

use super::*;

/// Edge of the cover on the current-track page, in pixels: 460 points on a 2× screen. The
/// thumbnails stop at 512 and would look soft at that size.
const LARGE_ARTWORK_PIXELS: u32 = 920;

/// Colors the current-track page after `image`, the LCD's cover. A track without a cover keeps
/// the neutral colors the page starts with.
pub(super) fn show_page_tint(state: &AppState<'_>, image: &slint::Image) {
    let tint = image
        .to_rgba8()
        .and_then(|pixels| crate::artwork_tint::dominant_color(pixels.as_bytes()))
        .map(crate::artwork_tint::page_tint);
    if let Some((top, bottom)) = tint {
        state.set_now_playing_tint(slint::Color::from_rgb_u8(top[0], top[1], top[2]));
        state.set_now_playing_tint_bottom(slint::Color::from_rgb_u8(
            bottom[0], bottom[1], bottom[2],
        ));
    }
}

impl UiBridge {
    /// Loads the current track's cover at the page's size, off the UI thread, only while the
    /// page is shown. The LCD's thumbnail stands in until it arrives.
    pub(super) fn show_large_artwork(&self) {
        use std::sync::atomic::Ordering;
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let generation = &self.artwork_slots.large_artwork_generation;
        let wanted_generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
        state.set_now_playing_artwork(slint::Image::default());
        let artwork = self.selected_track.get().and_then(|(album, _)| {
            self.albums
                .borrow()
                .get(album)
                .and_then(|album| album.artwork.clone())
        });
        let (Some(artwork), Some(database)) = (artwork, self.library_database.clone()) else {
            return;
        };
        let generation = generation.clone();
        let window = self.window.clone();
        std::thread::spawn(move || {
            let pixels = LibraryStore::open(&database)
                .ok()
                .and_then(|store| store.artwork_bytes(&artwork).ok().flatten())
                .and_then(|bytes| {
                    crate::artwork_cache::decode_scaled(&bytes, LARGE_ARTWORK_PIXELS)
                });
            let Some(pixels) = pixels else {
                return;
            };
            // Fails only once the event loop is gone: nobody is waiting for the cover then.
            let _ = slint::invoke_from_event_loop(move || {
                if generation.load(Ordering::Acquire) == wanted_generation
                    && let Some(window) = window.upgrade()
                {
                    window
                        .global::<AppState>()
                        .set_now_playing_artwork(slint::Image::from_rgba8(pixels));
                }
            });
        });
    }

    /// Shows track `track_index` of album `album_index` on the LCD and in the details.
    pub(super) fn show_current_track(
        &self,
        state: &AppState<'_>,
        album_index: usize,
        track_index: usize,
    ) {
        let albums = self.albums.borrow();
        let Some(album) = albums.get(album_index) else {
            return;
        };
        let Some(track) = album.tracks.get(track_index) else {
            return;
        };
        state.set_has_current_track(true);
        state.set_current_track_path(track.path.to_string_lossy().as_ref().into());
        state.set_current_artist(track.artist.as_str().into());
        state.set_current_album(album.title.as_str().into());
        state.set_current_format(presentation::codec_and_format(track).into());
        state.set_current_duration(presentation::clock(track.duration_seconds).into());
        state.set_current_play_count_label(presentation::play_count_label(track.play_count).into());
        state.set_current_filename(
            track
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref()
                .into(),
        );
        state.set_lcd_title(track.title.as_str().into());
        state.set_lcd_subtitle(format!("{} — {}", album.artist, album.title).into());
        self.show_lcd_artwork(album.artwork.as_ref());
        state.set_playing_album_index(album_index as i32);
        state.set_playing_track_index(track_index as i32);
        let path = track.path.clone();
        let cells = crate::presentation::track_info_cells(
            track,
            std::fs::metadata(&path).ok().map(|metadata| metadata.len()),
        );
        drop(albums);
        state.set_current_path(path.to_string_lossy().as_ref().into());
        self.show_track_info(state, cells, None);
        let is_page_shown = state.get_view() == crate::ViewKind::NowPlaying;
        self.show_authenticity_for(&path, is_page_shown);
        if is_page_shown {
            self.show_large_artwork();
        }
        self.publish_media_track();
    }

    /// Fills "Informations sur le fichier"; `analysis` adds what only the decode knows.
    pub(super) fn show_track_info(
        &self,
        state: &AppState<'_>,
        cells: Vec<(String, String)>,
        analysis: Option<&crate::authenticity::AuthenticityView>,
    ) {
        let mut cells: Vec<crate::DataCell> = cells
            .into_iter()
            .map(|(label, value)| crate::DataCell {
                label: label.into(),
                value: value.into(),
            })
            .collect();
        if let Some(view) = analysis {
            cells.extend(view.file_facts.iter().map(|fact| crate::DataCell {
                label: fact.label.as_str().into(),
                value: fact.text.as_str().into(),
            }));
        }
        state.set_track_info_cells(ModelRc::new(VecModel::from(cells)));
    }

    /// Shows the stored analysis of `path`, or starts one when `may_start_analysis` (the
    /// current-track page is shown): a track only listened to is never analyzed, so playing an
    /// album costs no decode and FFT burst at each track change.
    pub(super) fn show_authenticity_for(&self, path: &std::path::Path, may_start_analysis: bool) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        state.set_nyquist_note(crate::authenticity::NYQUIST_NOTE.into());
        state.set_authenticity_disclaimer(crate::authenticity::DISCLAIMER.into());
        let known = self
            .analyses
            .borrow()
            .iter()
            .find(|(analyzed, _)| analyzed == path)
            .map(|(_, outcome)| outcome.clone());
        match (known, &self.analysis_worker) {
            (Some(outcome), _) => self.show_authenticity(&state, &outcome),
            (None, Some(_)) if !may_start_analysis => {
                // Started when the page opens.
                state.set_authenticity_state(1);
                state.set_has_spectrogram(false);
            }
            (None, Some(worker)) => {
                state.set_authenticity_state(1);
                state.set_has_spectrogram(false);
                worker.request(path.to_path_buf());
            }
            (None, None) => {
                state.set_authenticity_state(3);
                state.set_authenticity_error("le module d’analyse n’a pas pu démarrer.".into());
                state.set_has_spectrogram(false);
            }
        }
    }

    /// Stores finished analyses and shows the one of the current track.
    pub(super) fn collect_analyses(&self) {
        /// Analyses kept in memory (a spectrogram is about a megabyte).
        const KEPT_ANALYSES: usize = 6;
        let Some(worker) = &self.analysis_worker else {
            return;
        };
        let outcomes = worker.take_results();
        if outcomes.is_empty() {
            return;
        }
        let current = self.current_path();
        for (path, outcome) in outcomes {
            if current.as_ref() == Some(&path)
                && let Some(window) = self.window.upgrade()
            {
                let state = window.global::<AppState>();
                self.show_authenticity(&state, &outcome);
                if let Ok(view) = &outcome {
                    let size = std::fs::metadata(&path).ok().map(|metadata| metadata.len());
                    let cells = self
                        .selected_track
                        .get()
                        .and_then(|(album, track)| {
                            self.albums
                                .borrow()
                                .get(album)?
                                .tracks
                                .get(track)
                                .map(|file| crate::presentation::track_info_cells(file, size))
                        })
                        .unwrap_or_default();
                    self.show_track_info(&state, cells, Some(view));
                }
            }
            let mut analyses = self.analyses.borrow_mut();
            analyses.retain(|(analyzed, _)| *analyzed != path);
            analyses.push_back((path, outcome));
            while analyses.len() > KEPT_ANALYSES {
                analyses.pop_front();
            }
        }
    }

    /// Puts one analysis on the current-track page.
    pub(super) fn show_authenticity(
        &self,
        state: &AppState<'_>,
        outcome: &Result<crate::authenticity::AuthenticityView, String>,
    ) {
        use crate::authenticity::VerdictTone;
        let view = match outcome {
            Ok(view) => view,
            Err(error) => {
                state.set_authenticity_state(3);
                state.set_authenticity_error(error.as_str().into());
                state.set_has_spectrogram(false);
                return;
            }
        };
        let cells = |items: &[crate::authenticity::LabeledText]| {
            ModelRc::new(VecModel::from(
                items
                    .iter()
                    .map(|item| crate::DataCell {
                        label: item.label.as_str().into(),
                        value: item.text.as_str().into(),
                    })
                    .collect::<Vec<_>>(),
            ))
        };
        state.set_authenticity_verdict(view.verdict.as_str().into());
        state.set_authenticity_tone(match view.tone {
            VerdictTone::Neutral => 0,
            VerdictTone::Good => 1,
            VerdictTone::Warning => 2,
        });
        state.set_authenticity_blurb(view.blurb.as_str().into());
        state.set_authenticity_strength(view.strength.as_str().into());
        state.set_authenticity_findings(cells(&view.findings));
        state.set_authenticity_measures(cells(&view.measures));
        state.set_authenticity_indicators(ModelRc::new(VecModel::from(
            view.indicators
                .iter()
                .map(|indicator| SharedString::from(indicator.as_str()))
                .collect::<Vec<_>>(),
        )));
        match &view.spectrogram {
            Some(pixels) => {
                let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &pixels.rgba,
                    pixels.width,
                    pixels.height,
                );
                state.set_spectrogram(slint::Image::from_rgba8(buffer));
                state.set_spectrogram_top_label(
                    format!(
                        "{} kHz",
                        crate::authenticity::number(view.spectrogram_max_hz / 1000.0, 0)
                    )
                    .into(),
                );
                state.set_spectrogram_middle_label(
                    format!(
                        "{} kHz",
                        crate::authenticity::number(view.spectrogram_max_hz / 2000.0, 0)
                    )
                    .into(),
                );
                state.set_spectrogram_end_label(
                    presentation::clock(view.duration_seconds.round() as u64).into(),
                );
                state.set_has_spectrogram(true);
            }
            None => state.set_has_spectrogram(false),
        }
        state.set_authenticity_state(2);
    }
}
