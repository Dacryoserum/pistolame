//! Données factices et palette partagées par les trois maquettes,
//! pour que la comparaison porte uniquement sur le framework d'interface.

pub mod artwork;
pub mod catalog;
pub mod playback;
pub mod spectrogram;
pub mod tokens;

pub const TRACK_COUNT: usize = 10_000;
pub const ALBUM_COUNT: usize = 812;
pub const SPECTRUM_BARS: usize = 48;

/// Palette en 0xRRGGBB.
pub mod color {
    pub const BG: u32 = 0x0B0C0F;
    pub const SURFACE: u32 = 0x14161B;
    pub const SURFACE_2: u32 = 0x1C1F26;
    pub const BORDER: u32 = 0x262A33;
    pub const TEXT: u32 = 0xE8E6E1;
    pub const MUTED: u32 = 0x8B8F98;
    pub const ACCENT: u32 = 0xD9A441;
    pub const OK: u32 = 0x4CC38A;
}

#[derive(Clone, Debug)]
pub struct Album {
    pub title: String,
    pub artist: String,
    pub year: u32,
    /// Deux couleurs 0xRRGGBB pour la pochette générée (dégradé).
    pub cover: (u32, u32),
}

#[derive(Clone, Debug)]
pub struct Track {
    pub number: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_index: usize,
    pub format: &'static str,
    pub duration_s: u32,
}

const ARTISTS: &[&str] = &[
    "Miles Davis",
    "Nils Frahm",
    "Daft Punk",
    "Björk",
    "Radiohead",
    "Bill Evans",
    "Hania Rani",
    "Massive Attack",
    "Air",
    "Portishead",
    "Ólafur Arnalds",
    "Keith Jarrett",
    "Bonobo",
    "Sade",
    "Pink Floyd",
    "Aphex Twin",
];
pub(crate) const WORDS: &[&str] = &[
    "Blue", "Night", "Silver", "Echo", "Glass", "River", "Velvet", "Static", "Amber", "Hollow",
    "Distant", "Midnight", "Paper", "Golden", "Quiet", "Electric", "Sky", "Lines", "Signal",
    "Ghost", "Bloom", "Motion", "Shore", "Fields",
];
const FORMATS: &[&str] = &[
    "FLAC 16/44.1",
    "FLAC 24/96",
    "FLAC 24/192",
    "FLAC 24/48",
    "DSD64",
];

/// Générateur pseudo-aléatoire déterministe (xorshift), pour avoir les mêmes données partout.
pub(crate) struct Rng(pub(crate) u64);
impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub(crate) fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
}

fn hsl(h: f32, s: f32, l: f32) -> u32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| (((v + m) * 255.0).round() as u32).min(255);
    (to(r) << 16) | (to(g) << 8) | to(b)
}

pub fn albums() -> Vec<Album> {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    (0..ALBUM_COUNT)
        .map(|_| {
            let h = (rng.next() % 360) as f32;
            Album {
                title: format!("{} {}", rng.pick(WORDS), rng.pick(WORDS)),
                artist: rng.pick(ARTISTS).to_string(),
                year: 1959 + (rng.next() % 66) as u32,
                cover: (hsl(h, 0.55, 0.45), hsl((h + 40.0) % 360.0, 0.6, 0.18)),
            }
        })
        .collect()
}

pub fn tracks(albums: &[Album]) -> Vec<Track> {
    let mut rng = Rng(0xD1B54A32D192ED03);
    (0..TRACK_COUNT)
        .map(|i| {
            let album_index = i / 12 % albums.len();
            let a = &albums[album_index];
            Track {
                number: (i % 12) as u32 + 1,
                title: format!("{} {}", rng.pick(WORDS), rng.pick(WORDS)),
                artist: a.artist.clone(),
                album: a.title.clone(),
                album_index,
                format: rng.pick(FORMATS),
                duration_s: 120 + (rng.next() % 420) as u32,
            }
        })
        .collect()
}

pub fn fmt_time(s: u32) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

/// Faux spectre animé (valeurs 0..1), calculé à chaque frame comme le ferait le vrai.
pub fn spectrum(t: f32) -> [f32; SPECTRUM_BARS] {
    let mut out = [0.0; SPECTRUM_BARS];
    for (i, v) in out.iter_mut().enumerate() {
        let f = i as f32 / SPECTRUM_BARS as f32;
        let envelope = (1.0 - f).powf(1.4) * 0.85 + 0.1;
        let wobble = ((t * (2.0 + f * 9.0) + i as f32 * 0.7).sin() * 0.5 + 0.5)
            * ((t * 1.3 + i as f32 * 0.31).cos() * 0.25 + 0.75);
        *v = (envelope * (0.35 + 0.65 * wobble)).clamp(0.02, 1.0);
    }
    out
}

/// Le chemin du signal affiché dans le panneau « En lecture ».
pub const SIGNAL_PATH: &[(&str, &str)] = &[
    ("Source", "FLAC 24 bit · 96 kHz"),
    ("Décodage", "Entier, sans conversion"),
    ("Sortie", "WASAPI exclusif · 24/96"),
    ("DAC", "FiiO K11"),
];
