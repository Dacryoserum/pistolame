#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{Color, Model as _, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

fn rgb(c: u32) -> Color {
    Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

struct Player {
    selected: usize,
    playing: bool,
    elapsed: f32,
    last: Instant,
}

fn main() -> Result<(), slint::PlatformError> {
    let started = Instant::now();
    let albums = common::albums();
    let tracks = common::tracks(&albums);

    let ui = AppWindow::new()?;

    let rows: Vec<TrackRow> = tracks
        .iter()
        .map(|t| TrackRow {
            number: t.number.to_string().into(),
            title: t.title.clone().into(),
            artist: t.artist.clone().into(),
            album: t.album.clone().into(),
            format: t.format.into(),
            duration: common::fmt_time(t.duration_s).into(),
        })
        .collect();
    ui.set_tracks(ModelRc::new(VecModel::from(rows)));
    ui.set_albums(ModelRc::new(VecModel::from(
        albums
            .iter()
            .take(40)
            .map(|a| AlbumCard {
                title: a.title.clone().into(),
                artist: a.artist.clone().into(),
                c1: rgb(a.cover.0),
                c2: rgb(a.cover.1),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_signal(ModelRc::new(VecModel::from(
        common::SIGNAL_PATH
            .iter()
            .map(|(l, v)| SignalStep { label: (*l).into(), value: (*v).into() })
            .collect::<Vec<_>>(),
    )));
    ui.set_track_count(common::TRACK_COUNT as i32);
    ui.set_album_count(common::ALBUM_COUNT as i32);

    let spectrum = Rc::new(VecModel::from(vec![0.0f32; common::SPECTRUM_BARS]));
    ui.set_spectrum(ModelRc::from(spectrum.clone()));

    let tracks = Rc::new(tracks);
    let albums = Rc::new(albums);
    let player = Rc::new(RefCell::new(Player { selected: 0, playing: true, elapsed: 0.0, last: Instant::now() }));

    let show_track = {
        let ui = ui.as_weak();
        let tracks = tracks.clone();
        let albums = albums.clone();
        move |i: usize| {
            let ui = ui.unwrap();
            let t = &tracks[i];
            let a = &albums[t.album_index];
            ui.set_now_title(t.title.clone().into());
            ui.set_now_artist(t.artist.clone().into());
            ui.set_now_album(t.album.clone().into());
            ui.set_now_format(SharedString::from(t.format));
            ui.set_now_c1(rgb(a.cover.0));
            ui.set_now_c2(rgb(a.cover.1));
            ui.set_total(common::fmt_time(t.duration_s).into());
            ui.set_selected(i as i32);
        }
    };
    show_track(0);

    ui.on_select({
        let player = player.clone();
        let show_track = show_track.clone();
        move |i| {
            let mut p = player.borrow_mut();
            p.selected = i as usize;
            p.elapsed = 0.0;
            p.playing = true;
            show_track(i as usize);
        }
    });
    ui.on_toggle_play({
        let player = player.clone();
        move || {
            let mut p = player.borrow_mut();
            p.playing = !p.playing;
            p.last = Instant::now();
        }
    });

    // Boucle d'animation ~60 i/s : spectre + progression.
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(16), {
        let ui = ui.as_weak();
        let player = player.clone();
        let tracks = tracks.clone();
        move || {
            let ui = ui.unwrap();
            let mut p = player.borrow_mut();
            let now = Instant::now();
            if p.playing {
                p.elapsed += (now - p.last).as_secs_f32();
            }
            p.last = now;
            let total = tracks[p.selected].duration_s as f32;
            if p.elapsed >= total {
                p.elapsed = 0.0;
            }
            ui.set_playing(p.playing);
            ui.set_progress(p.elapsed / total);
            ui.set_elapsed(common::fmt_time(p.elapsed as u32).into());
            if p.playing {
                let t = started.elapsed().as_secs_f32();
                for (i, v) in common::spectrum(t).into_iter().enumerate() {
                    spectrum.set_row_data(i, v);
                }
            }
        }
    });

    ui.run()
}
