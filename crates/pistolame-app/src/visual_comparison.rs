//! Deterministic visual comparison against the approved Slint prototype, without audio.
#[cfg(test)]
mod tests {
    use crate::*;
    use slint::{ComponentHandle, ModelRc, VecModel};
    use std::{cell::RefCell, io::Write, rc::Rc, time::Duration};

    #[test]
    #[ignore = "renders a deterministic comparison frame without audio"]
    fn render_comparison_fixture() -> Result<(), Box<dyn std::error::Error>> {
        let window = AppWindow::new()?;
        let is_light = std::env::var_os("PISTOLAME_COMPARE_LIGHT").is_some();
        let state = window.global::<AppState>();
        state.set_is_dark(!is_light);

        let (width, height) = if std::env::var_os("PISTOLAME_COMPARE_SMALL").is_some() {
            (1100.0, 700.0)
        } else {
            (1280.0, 820.0)
        };
        window
            .window()
            .set_size(slint::LogicalSize::new(width, height));
        // Navigation (now playing, Back) is owned by Rust and tested in `navigation.rs`; this
        // fixture only sets the view it renders.
        state.set_has_current_track(true);
        state.set_view(ViewKind::Grid);
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(32, 32);
        for (index, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
            *pixel =
                slint::Rgba8Pixel::new((index % 32 * 7) as u8, (index / 32 * 7) as u8, 90, 255);
        }
        let artwork = slint::Image::from_rgba8(pixels);
        let albums = (0..20)
            .map(|index| AlbumCard {
                catalog_index: index,
                title: format!("Album {}", index + 1).into(),
                artist: "Artiste témoin".into(),
                artwork: artwork.clone(),
            })
            .collect::<Vec<_>>();
        state.set_albums(ModelRc::new(VecModel::from(albums)));
        state.set_lcd_title("Morceau témoin".into());
        state.set_lcd_subtitle("Artiste témoin — Album 1".into());
        state.set_lcd_artwork(artwork.clone());
        state.set_is_playing(false);
        state.set_is_bit_perfect(true);
        state.set_badge_label("Bit-perfect · 24/96".into());
        state.set_elapsed_label("1:23".into());
        state.set_remaining_label("−3:33".into());
        state.set_progress(0.28);
        state.set_playing_album_index(0);
        state.set_playing_track_index(1);
        state.set_detail_album_index(0);
        state.set_detail_artists(ModelRc::new(VecModel::from(vec![
            slint::SharedString::from("Artiste témoin"),
        ])));
        state.set_detail(DetailInfo {
            title: "Album témoin".into(),
            artist: "Artiste témoin".into(),
            meta_line: "Classique · 2024 · FLAC 24 bits / 96 kHz".into(),
            footer_line: "10 morceaux · 49 minutes".into(),
            artwork,
            verdict_label: "Non analysé".into(),
            ..Default::default()
        });
        state.set_detail_tracks(ModelRc::new(VecModel::from(
            (0..10)
                .map(|index| TrackRowData {
                    number: index + 1,
                    title: format!("Morceau {}", index + 1).into(),
                    format_label: "24/96".into(),
                    duration_label: "4:56".into(),
                    track_index: index,
                    ..TrackRowData::default()
                })
                .collect::<Vec<_>>(),
        )));
        if std::env::var_os("PISTOLAME_COMPARE_DETAIL").is_some() {
            state.set_view(ViewKind::Detail);
        }
        if std::env::var_os("PISTOLAME_COMPARE_NOW_PLAYING").is_some() {
            state.set_current_artist("Artiste témoin".into());
            state.set_current_album("Album témoin".into());
            state.set_current_format("FLAC 24 bits / 96 kHz".into());
            state.set_current_duration("4:56".into());
            state.set_current_filename("02 - Morceau témoin.flac".into());
            state.set_transmission_text("Transmission : aucune lecture".into());
            state.set_integrity_text("Fichier : aucune lecture".into());
            state.set_view(ViewKind::NowPlaying);
            state.set_path_steps(ModelRc::new(VecModel::from(
                ["Source", "Décodage", "Volume", "Sortie", "DAC"]
                    .map(|label| PathStep {
                        label: label.into(),
                        value: "Aucune lecture".into(),
                        is_transparent: false,
                    })
                    .to_vec(),
            )));
            state.set_badge_label("Aucune lecture".into());
            state.set_is_bit_perfect(false);
        }
        let result = Rc::new(RefCell::new(None));
        let target = result.clone();
        let weak = window.as_weak();
        window.show()?;
        slint::Timer::single_shot(Duration::from_millis(600), move || {
            let capture = (|| -> Result<(), Box<dyn std::error::Error>> {
                let window = weak.upgrade().ok_or("closed comparison window")?;
                let pixels = window.window().take_snapshot()?;
                let path =
                    std::env::var_os("PISTOLAME_COMPARE_OUTPUT").ok_or("missing output path")?;
                let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
                write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
                for pixel in pixels.as_bytes().as_chunks::<4>().0 {
                    file.write_all(&pixel[..3])?;
                }
                file.flush()?;
                Ok(())
            })();
            *target.borrow_mut() = Some(capture);
            let _ = slint::quit_event_loop();
        });
        slint::run_event_loop()?;
        result
            .borrow_mut()
            .take()
            .ok_or("missing comparison capture")?
    }
}
