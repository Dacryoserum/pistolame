//! Fake library for the "maquette Apple Music" prototypes: albums with their own tracks,
//! formats, authenticity reports and signal path, plus every data-derived French label,
//! so both UIs print exactly the same strings.

use crate::artwork::ARTWORK_VARIANT_COUNT;
use crate::{ALBUM_COUNT, Rng, WORDS};

pub const OUTPUT_DEVICE_NAME: &str = "FiiO K11";
pub const OUTPUT_API_LABEL: &str = "WASAPI exclusif";
/// No-break space, required by French typography before ":" and inside « ».
pub const NO_BREAK_SPACE: char = '\u{a0}';
/// Typographic minus sign used for negative values (−6,0 dB, −2:41).
pub const MINUS_SIGN: char = '\u{2212}';

const FICTIONAL_ARTISTS: &[&str] = &[
    "Élise Marchand Trio",
    "Nordlys",
    "Les Ondes Basses",
    "Kasimir Vale",
    "Aurore Delacroix",
    "Halden Quartet",
    "Sami Okoro",
    "Mira Sølvberg",
    "The Glass Orchard",
    "Ondine Ferrat",
    "Port Cassis",
    "Yuki Arasaka",
    "Velours Noir",
    "Théo Lambert",
    "Cobalt Avenue",
    "Ingrid Holm Ensemble",
];
const GENRES: &[&str] = &[
    "Jazz",
    "Classique",
    "Électronique",
    "Rock",
    "Ambient",
    "Soul",
    "Folk",
    "Musique de film",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub bits_per_sample: u8,
    pub sample_rate_hz: u32,
}

impl AudioFormat {
    pub const CD: AudioFormat = AudioFormat {
        bits_per_sample: 16,
        sample_rate_hz: 44_100,
    };
    pub const STUDIO_48: AudioFormat = AudioFormat {
        bits_per_sample: 24,
        sample_rate_hz: 48_000,
    };
    pub const HIGH_RES_96: AudioFormat = AudioFormat {
        bits_per_sample: 24,
        sample_rate_hz: 96_000,
    };
    pub const HIGH_RES_192: AudioFormat = AudioFormat {
        bits_per_sample: 24,
        sample_rate_hz: 192_000,
    };

    /// "24/96", "16/44,1".
    pub fn short_label(self) -> String {
        format!(
            "{}/{}",
            self.bits_per_sample,
            format_kilohertz_number(self.sample_rate_hz)
        )
    }

    /// "FLAC 24 bits / 96 kHz".
    pub fn long_label(self) -> String {
        format!(
            "FLAC {} bits / {}",
            self.bits_per_sample,
            format_frequency(self.sample_rate_hz)
        )
    }

    pub fn is_high_resolution(self) -> bool {
        self.bits_per_sample > 16 || self.sample_rate_hz > 48_000
    }

    pub fn nyquist_hz(self) -> u32 {
        self.sample_rate_hz / 2
    }
}

/// 44100 -> "44,1", 96000 -> "96", 22050 -> "22,05".
fn format_kilohertz_number(frequency_hz: u32) -> String {
    let whole = frequency_hz / 1000;
    let hundredths = (frequency_hz % 1000 + 5) / 10;
    match hundredths {
        0 => format!("{whole}"),
        h if h % 10 == 0 => format!("{whole},{}", h / 10),
        h => format!("{whole},{h:02}"),
    }
}

/// French frequency label: "0 Hz", "800 Hz", "44,1 kHz", "22,05 kHz", "96 kHz".
pub fn format_frequency(frequency_hz: u32) -> String {
    if frequency_hz < 1000 {
        format!("{frequency_hz}{NO_BREAK_SPACE}Hz")
    } else {
        format!(
            "{}{NO_BREAK_SPACE}kHz",
            format_kilohertz_number(frequency_hz)
        )
    }
}

/// "3:07".
pub fn format_clock(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// "−2:41".
pub fn format_remaining_clock(seconds: u32) -> String {
    format!("{MINUS_SIGN}{}", format_clock(seconds))
}

/// "52 min", "1 h 04 min".
pub fn format_total_duration(seconds: u32) -> String {
    let minutes = (seconds + 30) / 60;
    if minutes < 60 {
        format!("{minutes}{NO_BREAK_SPACE}min")
    } else {
        format!(
            "{}{NO_BREAK_SPACE}h {:02}{NO_BREAK_SPACE}min",
            minutes / 60,
            minutes % 60
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthenticityFinding {
    Clean,
    UpsampledFromCd,
    PaddedTo24Bits,
    LossyTranscode,
}

#[derive(Clone, Debug)]
pub struct AuthenticityReport {
    pub finding: AuthenticityFinding,
    pub effective_bits_per_sample: u8,
    pub spectral_cutoff_hz: u32,
    pub dynamic_range_db: u8,
    pub clipped_sample_count: u32,
    pub md5_verified_track_count: u32,
    pub track_count: u32,
}

impl AuthenticityReport {
    pub fn is_authentic(&self) -> bool {
        self.finding == AuthenticityFinding::Clean
    }

    /// "Authentique" or "Douteux".
    pub fn verdict_label(&self) -> &'static str {
        if self.is_authentic() {
            "Authentique"
        } else {
            "Douteux"
        }
    }

    pub fn summary(&self, format: AudioFormat) -> String {
        let cutoff = format_frequency(self.spectral_cutoff_hz);
        let nbsp = NO_BREAK_SPACE;
        match self.finding {
            AuthenticityFinding::Clean if format.is_high_resolution() => format!(
                "Haute résolution authentique{nbsp}: contenu jusqu'à {cutoff}, {} bits réellement utilisés.",
                self.effective_bits_per_sample
            ),
            AuthenticityFinding::Clean => format!(
                "Qualité CD authentique{nbsp}: spectre complet jusqu'à {cutoff}, aucune trace de compression avec pertes."
            ),
            AuthenticityFinding::UpsampledFromCd => format!(
                "Fausse haute résolution{nbsp}: le spectre s'arrête à {cutoff} alors que le fichier annonce {}. Probablement un CD suréchantillonné.",
                format_frequency(format.sample_rate_hz)
            ),
            AuthenticityFinding::PaddedTo24Bits => format!(
                "Faux 24 bits{nbsp}: seuls 16 bits portent du signal, les 8 autres sont vides. Probablement un CD converti."
            ),
            AuthenticityFinding::LossyTranscode => format!(
                "Transcodage probable{nbsp}: coupure nette à {cutoff} et palier typique d'un MP3 ou d'un AAC."
            ),
        }
    }

    /// "24 bits" or "16 bits sur 24".
    pub fn effective_resolution_label(&self, format: AudioFormat) -> String {
        if self.effective_bits_per_sample < format.bits_per_sample {
            format!(
                "{} bits sur {}",
                self.effective_bits_per_sample, format.bits_per_sample
            )
        } else {
            format!("{} bits", self.effective_bits_per_sample)
        }
    }

    pub fn spectral_cutoff_label(&self) -> String {
        format_frequency(self.spectral_cutoff_hz)
    }

    /// "DR12".
    pub fn dynamic_range_label(&self) -> String {
        format!("DR{}", self.dynamic_range_db)
    }

    /// "Aucun", "1 échantillon", "148 échantillons".
    pub fn clipping_label(&self) -> String {
        match self.clipped_sample_count {
            0 => "Aucun".to_string(),
            1 => "1 échantillon".to_string(),
            count => format!("{count} échantillons"),
        }
    }

    /// "12/12 pistes conformes".
    pub fn md5_label(&self) -> String {
        format!(
            "{}/{} pistes conformes",
            self.md5_verified_track_count, self.track_count
        )
    }

    /// Cutoff as a fraction of the file's Nyquist frequency, in 0..=1.
    pub fn cutoff_fraction_of_nyquist(&self, format: AudioFormat) -> f32 {
        (self.spectral_cutoff_hz as f32 / format.nyquist_hz() as f32).clamp(0.0, 1.0)
    }

    /// The labelled cutoff line is drawn on the spectrogram only when it is evidence.
    pub fn should_mark_cutoff(&self) -> bool {
        !self.is_authentic() && self.finding != AuthenticityFinding::PaddedTo24Bits
    }
}

#[derive(Clone, Debug)]
pub struct CatalogTrack {
    pub number: u32,
    pub title: String,
    pub duration_seconds: u32,
    pub format: AudioFormat,
}

#[derive(Clone, Debug)]
pub struct CatalogAlbum {
    pub title: String,
    pub artist: String,
    pub genre: &'static str,
    pub year: u32,
    pub format: AudioFormat,
    pub artwork_variant: usize,
    pub tracks: Vec<CatalogTrack>,
    pub authenticity: AuthenticityReport,
}

impl CatalogAlbum {
    /// "Jazz · 1972 · FLAC 24 bits / 96 kHz".
    pub fn meta_line(&self) -> String {
        format!(
            "{} · {} · {}",
            self.genre,
            self.year,
            self.format.long_label()
        )
    }

    pub fn total_duration_seconds(&self) -> u32 {
        self.tracks.iter().map(|track| track.duration_seconds).sum()
    }

    /// "12 morceaux · 52 min".
    pub fn footer_line(&self) -> String {
        let count = self.tracks.len();
        let noun = if count == 1 { "morceau" } else { "morceaux" };
        format!(
            "{count} {noun} · {}",
            format_total_duration(self.total_duration_seconds())
        )
    }

    /// Case-insensitive match on title or artist; an empty query matches everything.
    pub fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.title.to_lowercase().contains(&query)
            || self.artist.to_lowercase().contains(&query)
    }

    /// "Nordlys — Blue Echo", the second line of the LCD.
    pub fn lcd_subtitle(&self) -> String {
        format!("{} — {}", self.artist, self.title)
    }
}

fn clean_cutoff_hz(format: AudioFormat) -> u32 {
    // A genuine master fills the band almost up to Nyquist; rounded to 100 Hz.
    (format.nyquist_hz() * 998 / 1000) / 100 * 100
}

fn build_report(
    finding: AuthenticityFinding,
    format: AudioFormat,
    track_count: u32,
    rng: &mut Rng,
) -> AuthenticityReport {
    let spectral_cutoff_hz = match finding {
        AuthenticityFinding::Clean | AuthenticityFinding::PaddedTo24Bits => clean_cutoff_hz(format),
        AuthenticityFinding::UpsampledFromCd => 22_050,
        AuthenticityFinding::LossyTranscode => 16_000,
    };
    let effective_bits_per_sample = if finding == AuthenticityFinding::PaddedTo24Bits {
        16
    } else {
        format.bits_per_sample
    };
    let clipped_sample_count = if rng.next().is_multiple_of(5) {
        20 + (rng.next() % 480) as u32
    } else {
        0
    };
    AuthenticityReport {
        finding,
        effective_bits_per_sample,
        spectral_cutoff_hz,
        dynamic_range_db: 6 + (rng.next() % 10) as u8,
        clipped_sample_count,
        md5_verified_track_count: track_count,
        track_count,
    }
}

fn pick_finding(format: AudioFormat, rng: &mut Rng) -> AuthenticityFinding {
    match rng.next() % 20 {
        0 if format.sample_rate_hz > 48_000 => AuthenticityFinding::UpsampledFromCd,
        1 if format.bits_per_sample == 24 => AuthenticityFinding::PaddedTo24Bits,
        2 => AuthenticityFinding::LossyTranscode,
        _ => AuthenticityFinding::Clean,
    }
}

fn generate_album(rng: &mut Rng) -> CatalogAlbum {
    const FORMAT_WEIGHTS: &[AudioFormat] = &[
        AudioFormat::CD,
        AudioFormat::CD,
        AudioFormat::STUDIO_48,
        AudioFormat::HIGH_RES_96,
        AudioFormat::HIGH_RES_96,
        AudioFormat::HIGH_RES_192,
    ];
    let title = format!("{} {}", rng.pick(WORDS), rng.pick(WORDS));
    let artist = rng.pick(FICTIONAL_ARTISTS).to_string();
    let genre = *rng.pick(GENRES);
    let year = 1959 + (rng.next() % 66) as u32;
    let format = *rng.pick(FORMAT_WEIGHTS);
    let artwork_variant = (rng.next() % ARTWORK_VARIANT_COUNT as u64) as usize;
    let track_count = 6 + (rng.next() % 9) as u32;
    let tracks = (1..=track_count)
        .map(|number| CatalogTrack {
            number,
            title: format!("{} {}", rng.pick(WORDS), rng.pick(WORDS)),
            duration_seconds: 150 + (rng.next() % 330) as u32,
            format,
        })
        .collect();
    let finding = pick_finding(format, rng);
    let authenticity = build_report(finding, format, track_count, rng);
    CatalogAlbum {
        title,
        artist,
        genre,
        year,
        format,
        artwork_variant,
        tracks,
        authenticity,
    }
}

/// The whole fake library, sorted by title (then artist), deterministic.
/// Fixtures: album 0 is an authentic 24/96, album 1 a fake hi-res 24/96, so both
/// verdicts are visible in the first grid row.
pub fn catalog_albums() -> Vec<CatalogAlbum> {
    let mut rng = Rng(0xA5A5_5A5A_DEAD_BEEF);
    let mut albums: Vec<CatalogAlbum> =
        (0..ALBUM_COUNT).map(|_| generate_album(&mut rng)).collect();
    albums.sort_by(|left, right| {
        (left.title.to_lowercase(), left.artist.to_lowercase())
            .cmp(&(right.title.to_lowercase(), right.artist.to_lowercase()))
    });
    for (album_index, finding) in [
        (0, AuthenticityFinding::Clean),
        (1, AuthenticityFinding::UpsampledFromCd),
    ] {
        let album = &mut albums[album_index];
        album.format = AudioFormat::HIGH_RES_96;
        for track in &mut album.tracks {
            track.format = AudioFormat::HIGH_RES_96;
        }
        let track_count = album.tracks.len() as u32;
        album.authenticity = build_report(finding, AudioFormat::HIGH_RES_96, track_count, &mut rng);
    }
    albums
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignalStep {
    /// "Source", "Décodage", "Volume", "Sortie", "DAC".
    pub label: &'static str,
    pub value: String,
    /// True when this step leaves the samples untouched.
    pub is_transparent: bool,
}

/// The mock pipeline is bit-perfect only at full digital volume.
pub fn is_bit_perfect(volume_percent: u8) -> bool {
    volume_percent >= 100
}

/// "−6,0 dB"; 0 % is "Muet".
pub fn volume_attenuation_label(volume_percent: u8) -> String {
    if volume_percent == 0 {
        return "Muet".to_string();
    }
    let decibels = 20.0 * (volume_percent.min(100) as f32 / 100.0).log10();
    let tenths = (decibels.abs() * 10.0).round() as u32;
    format!(
        "{MINUS_SIGN}{},{}{NO_BREAK_SPACE}dB",
        tenths / 10,
        tenths % 10
    )
}

pub fn signal_path(format: AudioFormat, volume_percent: u8) -> Vec<SignalStep> {
    let bits = format.bits_per_sample;
    let rate = format_frequency(format.sample_rate_hz);
    let volume_value = if is_bit_perfect(volume_percent) {
        format!("100{NO_BREAK_SPACE}% · aucun traitement")
    } else {
        format!(
            "{} · calcul en flottant",
            volume_attenuation_label(volume_percent)
        )
    };
    vec![
        SignalStep {
            label: "Source",
            value: format!("FLAC {bits} bits · {rate}"),
            is_transparent: true,
        },
        SignalStep {
            label: "Décodage",
            value: format!("Entier {bits} bits, sans conversion"),
            is_transparent: true,
        },
        SignalStep {
            label: "Volume",
            value: volume_value,
            is_transparent: is_bit_perfect(volume_percent),
        },
        SignalStep {
            label: "Sortie",
            value: format!("{OUTPUT_API_LABEL} · {bits} bits · {rate}"),
            is_transparent: true,
        },
        SignalStep {
            label: "DAC",
            value: OUTPUT_DEVICE_NAME.to_string(),
            is_transparent: true,
        },
    ]
}

/// Texts of the quality badge and of the popover header.
#[derive(Clone, Debug, PartialEq)]
pub struct SignalSummary {
    pub is_bit_perfect: bool,
    /// "Bit-perfect · 24/96" or "Traité · 24/96".
    pub badge_label: String,
    /// "Bit-perfect" or "Signal traité".
    pub title: &'static str,
    pub description: String,
}

pub fn signal_summary(format: AudioFormat, volume_percent: u8) -> SignalSummary {
    let is_bit_perfect = is_bit_perfect(volume_percent);
    let short = format.short_label();
    if is_bit_perfect {
        SignalSummary {
            is_bit_perfect,
            badge_label: format!("Bit-perfect · {short}"),
            title: "Bit-perfect",
            description: "Les échantillons arrivent au DAC sans aucune modification.".to_string(),
        }
    } else {
        SignalSummary {
            is_bit_perfect,
            badge_label: format!("Traité · {short}"),
            title: "Signal traité",
            description: format!(
                "Le volume numérique ({}) recalcule chaque échantillon. Remettez le volume à 100{NO_BREAK_SPACE}% pour retrouver le bit-perfect.",
                volume_attenuation_label(volume_percent)
            ),
        }
    }
}

/// « query » with no-break spaces, for the empty search result message.
pub fn no_results_message(query: &str) -> String {
    format!(
        "Aucun album ne correspond à «{NO_BREAK_SPACE}{}{NO_BREAK_SPACE}».",
        query.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_use_decimal_comma() {
        assert_eq!(format_frequency(44_100), "44,1\u{a0}kHz");
        assert_eq!(format_frequency(22_050), "22,05\u{a0}kHz");
        assert_eq!(format_frequency(96_000), "96\u{a0}kHz");
        assert_eq!(format_frequency(47_900), "47,9\u{a0}kHz");
        assert_eq!(format_frequency(0), "0\u{a0}Hz");
        assert_eq!(AudioFormat::CD.short_label(), "16/44,1");
    }

    #[test]
    fn durations_and_volume_labels() {
        assert_eq!(format_total_duration(52 * 60), "52\u{a0}min");
        assert_eq!(format_total_duration(64 * 60), "1\u{a0}h 04\u{a0}min");
        assert_eq!(format_remaining_clock(161), "\u{2212}2:41");
        assert_eq!(volume_attenuation_label(50), "\u{2212}6,0\u{a0}dB");
        assert_eq!(volume_attenuation_label(0), "Muet");
    }

    #[test]
    fn catalog_is_deterministic_with_fixtures() {
        let albums = catalog_albums();
        assert_eq!(albums.len(), ALBUM_COUNT);
        assert!(albums[0].authenticity.is_authentic());
        assert_eq!(
            albums[1].authenticity.finding,
            AuthenticityFinding::UpsampledFromCd
        );
        assert_eq!(albums[1].authenticity.spectral_cutoff_hz, 22_050);
        assert_eq!(catalog_albums()[42].title, albums[42].title);
    }

    #[test]
    fn signal_path_flags_volume_processing() {
        let path = signal_path(AudioFormat::HIGH_RES_96, 50);
        assert_eq!(path.len(), 5);
        assert!(!path[2].is_transparent);
        assert!(!signal_summary(AudioFormat::HIGH_RES_96, 50).is_bit_perfect);
        assert!(signal_summary(AudioFormat::HIGH_RES_96, 100).is_bit_perfect);
    }
}
