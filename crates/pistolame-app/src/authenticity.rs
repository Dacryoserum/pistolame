//! The authenticity section of the current-track page: Nyquist's analysis
//! (`pistolame_analysis`) rendered in French, and its spectrogram turned into pixels. Pure
//! logic; the UI bridge runs the analysis on a worker and shows the result.
//!
//! The wording follows Nyquist's French interface (src/lib/i18n.svelte.ts): a four-state
//! verdict that is never a flat yes/no, evidence stated one sentence per indicator, and
//! "indéterminé" as the honest default.

use base64::Engine;
use pistolame_analysis::analysis::AnalysisResult;
use pistolame_analysis::mdct_grid::MdctWindow;
use pistolame_analysis::transcode_detect::{IndicatorDetail, Verdict};

/// How the verdict is colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerdictTone {
    /// Indeterminate or a declared lossy format: no accusation, no reassurance.
    Neutral,
    Good,
    Warning,
}

/// One labeled value or one titled finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LabeledText {
    pub(crate) label: String,
    pub(crate) text: String,
}

/// Everything the authenticity section shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AuthenticityView {
    pub(crate) verdict: String,
    pub(crate) tone: VerdictTone,
    pub(crate) blurb: String,
    /// "force des indices : modérée", empty when the verdict is not an inference.
    pub(crate) strength: String,
    pub(crate) indicators: Vec<String>,
    /// Separate problems: padded bit depth, limited bandwidth, clipping, damaged file.
    pub(crate) findings: Vec<LabeledText>,
    pub(crate) measures: Vec<LabeledText>,
    /// File facts only the decode knows (channels, samples, checksum).
    pub(crate) file_facts: Vec<LabeledText>,
    pub(crate) spectrogram: Option<SpectrogramPixels>,
    /// Highest frequency of the spectrogram, in Hz (its top edge).
    pub(crate) spectrogram_max_hz: f64,
    pub(crate) duration_seconds: f64,
}

/// An RGBA image, row-major, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpectrogramPixels {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// The note under the section, pointing to the full tool.
pub(crate) const NYQUIST_NOTE: &str = "Analyse issue de Nyquist, du même auteur. Pour le rapport \
complet (spectre détaillé, image stéréo, comparaison de fichiers, export JSON), ouvrez ce fichier \
dans Nyquist.";
/// Where "Nyquist" leads.
pub(crate) const NYQUIST_URL: &str = "https://github.com/Dacryoserum/nyquist";
/// Nyquist's standing disclaimer, shortened.
pub(crate) const DISCLAIMER: &str = "Un résultat propre n'est jamais une preuve de provenance : \
« indéterminé » est la réponse honnête quand rien ne permet de trancher. La force des indices est \
une appréciation qualitative, pas une probabilité.";

/// Renders `result` for the current-track page.
pub(crate) fn view_of(result: &AnalysisResult) -> AuthenticityView {
    let assessment = &result.transcode_assessment;
    let (verdict, tone, blurb) = match assessment.verdict {
        Verdict::ProbablyAuthentic => (
            "Probablement authentique".to_owned(),
            VerdictTone::Good,
            "Un élément positif a été mesuré, qui écarte une source avec perte à la fréquence du CD.",
        ),
        Verdict::ProbablyTranscoded => (
            "Probablement transcodé".to_owned(),
            VerdictTone::Warning,
            "Ceci ressemble à de l'audio avec perte encapsulé dans un conteneur sans perte.",
        ),
        Verdict::Indeterminate => (
            "Indéterminé".to_owned(),
            VerdictTone::Neutral,
            "Les indices disponibles ne permettent pas d'établir la provenance. Ce n'est ni une garantie d'authenticité, ni une accusation.",
        ),
        Verdict::DeclaredLossy => (
            format!(
                "Format avec perte ({})",
                result.file_info.codec.to_uppercase()
            ),
            VerdictTone::Neutral,
            "Ce fichier est dans un format avec perte et ne prétend pas le contraire : il n'y a rien à démasquer.",
        ),
    };
    AuthenticityView {
        verdict,
        tone,
        blurb: blurb.to_owned(),
        strength: assessment
            .confidence_score
            .map(|score| format!("Force des indices : {}", strength_label(score)))
            .unwrap_or_default(),
        indicators: assessment
            .indicators
            .iter()
            .map(|indicator| indicator_text(&indicator.detail))
            .collect(),
        findings: findings_of(result),
        measures: measures_of(result),
        file_facts: file_facts_of(result),
        spectrogram: spectrogram_pixels(result),
        spectrogram_max_hz: result.spectral_analysis.spectrogram.max_frequency_hz,
        duration_seconds: result.file_info.duration_seconds,
    }
}

/// Nyquist's qualitative reading of the evidence score (never shown as a percentage).
fn strength_label(score: f64) -> &'static str {
    if score >= 0.7 {
        "forte"
    } else if score >= 0.4 {
        "modérée"
    } else {
        "faible"
    }
}

/// One indicator in French, from its code and measurements.
fn indicator_text(detail: &IndicatorDetail) -> String {
    match detail {
        IndicatorDetail::EncoderTagMatched {
            tag_key,
            tag_value,
            matched_pattern,
            additional_tags,
        } => format!(
            "Le tag d'encodeur « {tag_key} » vaut « {tag_value} », ce qui correspond à l'encodeur exclusivement avec perte « {matched_pattern} »{}.",
            match additional_tags {
                0 => String::new(),
                1 => ", plus un autre tag correspondant".to_owned(),
                count => format!(", plus {count} autres tags correspondants"),
            }
        ),
        IndicatorDetail::TagIsOnlyEvidence => "Un tag peut être recopié, obsolète ou faux. Sans confirmation structurelle, il ne suffit pas à conclure à un transcodage.".to_owned(),
        IndicatorDetail::InsufficientSignal => "Le signal est silencieux ou trop faible pour fournir des indices spectraux fiables.".to_owned(),
        IndicatorDetail::IntegrityMismatch => "La somme de contrôle ne correspond pas à l'audio décodé. Le verdict est suspendu ; vérifiez ou ré-extrayez le fichier.".to_owned(),
        IndicatorDetail::InvalidSampleRate => "Fréquence d'échantillonnage invalide ; impossible d'évaluer le contenu spectral.".to_owned(),
        IndicatorDetail::SharpRolloff { steepness_db_per_khz, edge_khz } => format!(
            "Coupure franche (~{} dB/kHz) autour de {} kHz. Un encodeur, un filtre de mastering ou un ré-échantillonnage peuvent tous la produire : ce n'est pas une preuve de compression avec perte.",
            number(*steepness_db_per_khz, 0),
            number(*edge_khz, 1)
        ),
        IndicatorDetail::NoEncoderLowpass { scanned_from_khz, nyquist_khz } => format!(
            "Aucune coupure franche et soutenue entre {} et {} kHz. L'absence de coupure ne prouve pas l'absence de compression avec perte.",
            number(*scanned_from_khz, 0),
            number(*nyquist_khz, 1)
        ),
        IndicatorDetail::TransparentEncodeUnseen => "Une compression avec perte peut ne laisser aucun indice détectable par ces tests, notamment après traitement du signal.".to_owned(),
        IndicatorDetail::GradualRolloff { cutoff_khz, steepness_db_per_khz } => format!(
            "Le contenu s'arrête autour de {} kHz, mais la transition est progressive (~{} dB/kHz), pas le mur quasi vertical d'un codec. Compatible avec un master sombre, un report de vinyle ou de bande, ou un encodage avec perte : pas de quoi trancher.",
            number(*cutoff_khz, 1),
            number(*steepness_db_per_khz, 0)
        ),
        IndicatorDetail::MdctGridAligned { z_score, confirmed_z_score, window, frame_offset, zero_percent, baseline_percent } => format!(
            "Alignement MDCT compatible avec des blocs longs AAC (fenêtre {}, décalage {frame_offset}). Écart : {} au balayage, {} en confirmation ; {} % de coefficients quasi nuls contre {} % aux décalages témoins. Indice structurel fort, pas une preuve absolue.",
            match window {
                MdctWindow::KaiserBessel => "Kaiser–Bessel",
                MdctWindow::Sine => "sinus",
            },
            number(*z_score, 0),
            number(*confirmed_z_score, 0),
            number(*zero_percent, 1),
            number(*baseline_percent, 1)
        ),
        IndicatorDetail::MdctGridClear => "Aucun alignement AAC confirmé pour les fenêtres testées. Cela n'exclut pas une source AAC ; le MP3 n'est pas couvert par ce test.".to_owned(),
        IndicatorDetail::ContentAboveCdCeiling { level_db, ceiling_khz } => format!(
            "Énergie mesurée au-dessus de {} kHz : {} dB par rapport à la bande de référence. Du bruit ajouté après transcodage peut la recréer : elle n'établit pas l'authenticité.",
            number(*ceiling_khz, 2),
            number(*level_db, 0)
        ),
        IndicatorDetail::DeclaredLossyCodec { codec } => format!(
            "Ce fichier est en {}, un format avec perte qui ne se fait pas passer pour autre chose : il n'y a pas de transcodage à détecter.",
            codec.to_uppercase()
        ),
        IndicatorDetail::DecodeIncomplete { skipped_packets, stopped_early, channels_unequal } => format!(
            "Une partie de l'audio n'est jamais parvenue à l'analyse : {}. Aucun verdict sur le fichier entier ne peut être rendu.",
            if *channels_unequal {
                "les canaux ont des longueurs différentes".to_owned()
            } else if *skipped_packets == 0 {
                "le décodage s'est arrêté en cours de fichier".to_owned()
            } else if *stopped_early {
                format!("{skipped_packets} paquet(s) illisible(s), puis décodage interrompu")
            } else {
                format!("{skipped_packets} paquet(s) illisible(s) ignoré(s)")
            }
        ),
    }
}

/// Problems that stand apart from the transcode question.
fn findings_of(result: &AnalysisResult) -> Vec<LabeledText> {
    let mut findings = Vec::new();
    if result.file_info.integrity_verified == Some(false) {
        findings.push(LabeledText {
            label: "Somme de contrôle invalide".to_owned(),
            text: "L'audio ne correspond pas à la somme de contrôle stockée dans le fichier : il a été tronqué, modifié ou corrompu depuis sa création.".to_owned(),
        });
    }
    let bit_depth = &result.bit_depth_analysis;
    if let (Some(declared), Some(effective)) =
        (bit_depth.declared_bit_depth, bit_depth.effective_bit_depth)
        && effective < declared
    {
        findings.push(LabeledText {
            label: format!("Conteneur {declared} bits contenant de l'audio {effective} bits"),
            text: format!(
                "Sur les {} % d'échantillons non silencieux, tous tombent sur la grille à {effective} bits : la résolution supplémentaire ne porte aucune information. Compatible avec un simple rembourrage.",
                number(bit_depth.active_sample_ratio * 100.0, 0)
            ),
        });
    }
    let sample_rate = &result.sample_rate_analysis;
    if sample_rate.bandwidth_limited
        && let (Some(bandwidth), Some(ratio)) = (
            sample_rate.content_bandwidth_hz,
            sample_rate.bandwidth_ratio,
        )
    {
        findings.push(LabeledText {
            label: format!(
                "Bande limitée à {} kHz (fichier {} kHz)",
                number(bandwidth / 1000.0, 1),
                number(f64::from(sample_rate.declared_sample_rate_hz) / 1000.0, 1)
            ),
            text: format!(
                "La limite mesurée représente {} % de la bande disponible. Cela peut venir d'un filtrage natif ou d'un sur-échantillonnage.",
                number(ratio * 100.0, 0)
            ),
        });
    }
    let signal = &result.signal_analysis;
    if signal.clipped_run_count_total > 0 {
        findings.push(LabeledText {
            label: format!(
                "{} passage(s) aplati(s) au plein échelle",
                signal.clipped_run_count_total
            ),
            text: "Des échantillons consécutifs plaqués au plein échelle : compatible avec un écrêtage.".to_owned(),
        });
    }
    findings
}

/// The measurement grid.
fn measures_of(result: &AnalysisResult) -> Vec<LabeledText> {
    let measure = |label: &str, text: String| LabeledText {
        label: label.to_owned(),
        text,
    };
    let signal = &result.signal_analysis;
    let bit_depth = &result.bit_depth_analysis;
    let not_available = || "n/d".to_owned();
    vec![
        measure(
            "Résolution effective",
            bit_depth
                .effective_bit_depth
                .map_or_else(not_available, |bits| format!("{bits} bits")),
        ),
        measure(
            "Bande passante",
            result.spectral_analysis.spectral_cutoff_hz.map_or_else(
                || "aucune limite mesurable".to_owned(),
                |hz| format!("{} kHz", number(hz / 1000.0, 1)),
            ),
        ),
        measure(
            "Plage dynamique",
            result
                .dynamic_range
                .dr14
                .map_or_else(not_available, |dr| format!("DR{dr} ({})", dr_label(dr))),
        ),
        measure(
            "Sonie intégrée",
            signal
                .lufs_integrated
                .map_or_else(not_available, |lufs| format!("{} LUFS", number(lufs, 1))),
        ),
        measure(
            "Crête réelle",
            format!("{} dBTP", number(signal.true_peak_dbtp, 1)),
        ),
        measure(
            "Plage de sonie",
            signal
                .loudness_range_lu
                .map_or_else(not_available, |range| format!("{} LU", number(range, 1))),
        ),
        measure(
            "Passages aplatis",
            signal.clipped_run_count_total.to_string(),
        ),
    ]
}

/// Nyquist's reading of the DR scale.
fn dr_label(dr: i32) -> &'static str {
    match dr {
        14.. => "très élevée",
        12..=13 => "élevée",
        8..=11 => "moyenne",
        _ => "faible",
    }
}

/// What the decode learned about the file itself.
fn file_facts_of(result: &AnalysisResult) -> Vec<LabeledText> {
    let info = &result.file_info;
    vec![
        LabeledText {
            label: "Canaux".to_owned(),
            text: match info.channels {
                1 => "Mono".to_owned(),
                2 => "Stéréo".to_owned(),
                count => format!("{count} canaux"),
            },
        },
        LabeledText {
            label: "Échantillons".to_owned(),
            text: grouped(info.sample_count as u64),
        },
        LabeledText {
            label: "Somme de contrôle MD5".to_owned(),
            text: match info.integrity_verified {
                Some(true) => "Vérifiée".to_owned(),
                Some(false) => "Invalide".to_owned(),
                None => "Aucune stockée".to_owned(),
            },
        },
    ]
}

/// Nyquist's default "inferno" ramp: perceptually ordered, so faint content stays visible
/// and the picture invents no contours.
const INFERNO: [(f32, [u8; 3]); 5] = [
    (0.0, [0, 0, 4]),
    (0.25, [87, 16, 110]),
    (0.5, [188, 55, 84]),
    (0.75, [249, 142, 9]),
    (1.0, [252, 255, 164]),
];

fn inferno(intensity: u8) -> [u8; 3] {
    let position = f32::from(intensity) / 255.0;
    for pair in INFERNO.windows(2) {
        let ((start, low), (end, high)) = (pair[0], pair[1]);
        if position <= end {
            let fraction = (position - start) / (end - start);
            let mix = |channel: usize| {
                (f32::from(low[channel])
                    + (f32::from(high[channel]) - f32::from(low[channel])) * fraction)
                    .round() as u8
            };
            return [mix(0), mix(1), mix(2)];
        }
    }
    INFERNO[INFERNO.len() - 1].1
}

/// The spectrogram as an image: time left to right, frequency bottom to top.
fn spectrogram_pixels(result: &AnalysisResult) -> Option<SpectrogramPixels> {
    let data = &result.spectral_analysis.spectrogram;
    let (columns, rows) = (data.time_bin_count, data.frequency_bin_count);
    let intensities = base64::engine::general_purpose::STANDARD
        .decode(&data.intensity_base64)
        .ok()?;
    if columns == 0 || rows == 0 || intensities.len() < columns * rows {
        return None;
    }
    let mut rgba = vec![0u8; columns * rows * 4];
    for column in 0..columns {
        for row in 0..rows {
            // Input is [time][frequency] with frequency rising; the image's top row is the
            // highest frequency.
            let color = inferno(intensities[column * rows + row]);
            let pixel = ((rows - 1 - row) * columns + column) * 4;
            rgba[pixel..pixel + 3].copy_from_slice(&color);
            rgba[pixel + 3] = u8::MAX;
        }
    }
    Some(SpectrogramPixels {
        width: u32::try_from(columns).ok()?,
        height: u32::try_from(rows).ok()?,
        rgba,
    })
}

/// A French number: decimal comma, `decimals` digits.
pub(crate) fn number(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}").replace('.', ",")
}

/// Thousands grouped with a narrow no-break space, as French typography does.
pub(crate) fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_use_french_typography() {
        assert_eq!(number(23.456, 1), "23,5");
        assert_eq!(number(-2.0, 0), "-2");
        assert_eq!(grouped(7_175_041), "7\u{202f}175\u{202f}041");
        assert_eq!(grouped(512), "512");
    }

    #[test]
    fn the_colormap_runs_from_dark_to_pale() {
        assert_eq!(inferno(0), [0, 0, 4]);
        assert_eq!(inferno(255), [252, 255, 164]);
        let middle = inferno(128);
        assert!(middle[0] > 150 && middle[0] < 200, "{middle:?}");
    }

    #[test]
    fn indicators_read_in_french_with_their_measurements() {
        let text = indicator_text(&IndicatorDetail::SharpRolloff {
            steepness_db_per_khz: 41.6,
            edge_khz: 16.04,
        });
        assert!(text.starts_with("Coupure franche (~42 dB/kHz) autour de 16,0 kHz."));
        let text = indicator_text(&IndicatorDetail::DecodeIncomplete {
            skipped_packets: 3,
            stopped_early: false,
            channels_unequal: false,
        });
        assert!(text.contains("3 paquet(s) illisible(s) ignoré(s)"));
    }

    #[test]
    fn evidence_strength_is_never_a_percentage() {
        assert_eq!(strength_label(0.3), "faible");
        assert_eq!(strength_label(0.55), "modérée");
        assert_eq!(strength_label(0.9), "forte");
        assert_eq!(dr_label(15), "très élevée");
        assert_eq!(dr_label(6), "faible");
    }
}
