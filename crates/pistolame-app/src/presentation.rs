//! Pure French presentation of engine facts; no optimistic verification defaults.
use pistolame_core::*;

pub(crate) fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub(crate) fn remaining_clock(seconds: u64) -> String {
    format!("−{}", clock(seconds))
}

pub(crate) fn frequency(rate: u32) -> String {
    format!("{}", f64::from(rate) / 1000.0).replace('.', ",")
}

pub(crate) fn digest(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether the only obstacle to bit-perfect is the exact float delivery of a float-only device:
/// held exclusively at the source format, shown as "Exclusif", never as "Traité".
fn is_exclusive_with_exact_float_delivery(value: &SignalPathAssessment) -> bool {
    matches!(
        &value.verdict,
        BitPerfectVerdict::NotBitPerfect { reasons }
            if reasons == &[NotBitPerfectReason::DeliveredAsFloatToPlatform]
    )
}

/// A PCM format as audiophiles read it: "24 bits / 48 kHz", "16 bits / 44,1 kHz".
pub(crate) fn audio_format(bits_per_sample: u32, sample_rate_hz: u32) -> String {
    format!("{bits_per_sample} bits / {} kHz", frequency(sample_rate_hz))
}

/// A track's codec and format as the LCD and lists show it: "FLAC 24 bits / 96 kHz", or
/// "MP3 44,1 kHz" for lossy codecs, which have no bit depth of their own.
pub(crate) fn codec_and_format(track: &pistolame_library::MusicFile) -> String {
    if track.is_lossy || track.bits_per_sample == 0 {
        format!("{} {} kHz", track.codec, frequency(track.sample_rate_hz))
    } else {
        format!(
            "{} {}",
            track.codec,
            audio_format(track.bits_per_sample, track.sample_rate_hz)
        )
    }
}

/// The source as the badge and the popover name it: bits and rate, or for a lossy codec its
/// name and rate (the 24 bits it is rounded to are not the file's).
fn source_description(value: &SignalPathAssessment) -> String {
    let source = value.source_format;
    if value.source_codec.is_lossy {
        format!(
            "{} · {} kHz",
            value.source_codec.display_name,
            frequency(source.sample_rate_hz)
        )
    } else {
        audio_format(u32::from(source.bits_per_sample), source.sample_rate_hz)
    }
}

/// A PCM format in the short form the LCD and the album tiles have room for: "24/96",
/// "16/44,1". Lossy codecs, which have no bit depth of their own, show their name ("MP3").
pub(crate) fn compact_format(
    bits_per_sample: u32,
    sample_rate_hz: u32,
    lossy_codec_name: Option<&str>,
) -> String {
    match lossy_codec_name {
        Some(codec) => codec.to_owned(),
        None => format!("{bits_per_sample}/{}", frequency(sample_rate_hz)),
    }
}

/// What the grid knows of an album to tell copies apart.
pub(crate) struct AlbumIdentity<'album> {
    pub(crate) title: &'album str,
    pub(crate) artist: &'album str,
    /// Compact format of its first track ("24/48").
    pub(crate) format: String,
    pub(crate) track_count: usize,
}

/// For each album, what tells it apart when another album has the same title and artist
/// (ignoring case): its format when the copies differ there (a CD rip and a hi-res download),
/// else its number of tracks ("14 titres", shorter than "morceaux" under a tile). `None` for
/// an album without a namesake.
pub(crate) fn distinguishing_labels(albums: &[AlbumIdentity<'_>]) -> Vec<Option<String>> {
    let keys: Vec<(String, String)> = albums
        .iter()
        .map(|album| (album.title.to_lowercase(), album.artist.to_lowercase()))
        .collect();
    let mut formats_by_key: std::collections::HashMap<&(String, String), Vec<&str>> =
        std::collections::HashMap::with_capacity(keys.len());
    for (key, album) in keys.iter().zip(albums) {
        formats_by_key.entry(key).or_default().push(&album.format);
    }
    keys.iter()
        .zip(albums)
        .map(|(key, album)| {
            let formats = formats_by_key.get(key)?;
            if formats.len() < 2 {
                return None;
            }
            let is_format_shared = formats.iter().all(|format| *format == album.format);
            Some(if is_format_shared {
                format!(
                    "{} titre{}",
                    album.track_count,
                    if album.track_count == 1 { "" } else { "s" }
                )
            } else {
                album.format.clone()
            })
        })
        .collect()
}

/// The line under an album tile: the artist, plus what tells copies apart ("Maes · 24/48").
pub(crate) fn album_tile_subtitle(artist: &str, distinguishing_label: Option<&str>) -> String {
    match distinguishing_label {
        Some(label) => format!("{artist} · {label}"),
        None => artist.to_owned(),
    }
}

/// The verdict word the badge starts with: "Bit-perfect", "Mode partagé"…
fn verdict_word(value: &SignalPathAssessment) -> &'static str {
    if value.verdict == BitPerfectVerdict::BitPerfect {
        "Bit-perfect"
    } else if value.source_codec.is_lossy {
        "Avec perte"
    } else if value.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer {
        "Mode partagé"
    } else if is_exclusive_with_exact_float_delivery(value) {
        "Exclusif"
    } else {
        "Traité"
    }
}

pub(crate) fn assessment(value: &SignalPathAssessment) -> (String, String, bool) {
    let source = value.source_format;
    let format = source_description(value);
    let is_perfect = value.verdict == BitPerfectVerdict::BitPerfect;
    let shared = value.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer;
    let badge = format!("{} · {format}", verdict_word(value));
    let decoding = if value.source_codec.is_decoded_as_float {
        format!(
            "{} décodé en flottant, arrondi à 24 bits : ce ne sont pas les échantillons d’origine. ",
            value.source_codec.display_name
        )
    } else {
        String::new()
    };
    let backend = value.negotiated.backend_name;
    let reason = if shared {
        let cause = match value.negotiated.shared_mode_cause {
            Some(SharedModeCause::ExclusiveModeDisabledBySystem) => {
                "Mode exclusif désactivé par le système. "
            }
            Some(SharedModeCause::DeviceBusyInSharedMode) => {
                "Périphérique occupé par d’autres applications. "
            }
            Some(SharedModeCause::ExclusiveFormatRefused) => "Format refusé en mode exclusif. ",
            _ => "",
        };
        let mix = value
            .negotiated
            .system_mix_format
            .map(|mix| {
                format!(
                    " Mixeur : {} bits{} / {} kHz.",
                    mix.bits_per_sample,
                    if mix.is_float { " flottants" } else { "" },
                    frequency(mix.sample_rate_hz)
                )
            })
            .unwrap_or_default();
        format!(
            "{decoding}{cause}Le système peut convertir, rééchantillonner ou mixer le signal ; le bit-perfect n’est pas garanti.{mix}"
        )
    } else if value.negotiated.sample_delivery == DeviceSampleDelivery::ExactFloat32Conversion {
        format!(
            "{decoding}{} bits / {} kHz → flottants 32 bits, conversion exacte / {} kHz · {backend} exclusif. Ce périphérique n’accepte que des flottants : le système les traite ensuite, ce chemin n’est pas vérifiable jusqu’au DAC.",
            source.bits_per_sample,
            frequency(source.sample_rate_hz),
            frequency(value.negotiated.render_format.sample_rate_hz)
        )
    } else {
        let output = value.negotiated.render_format;
        format!(
            "{decoding}{} bits / {} kHz → {} bits dans {} / {} kHz · {backend} exclusif",
            source.bits_per_sample,
            frequency(source.sample_rate_hz),
            output.valid_bits_per_sample,
            output.container_bytes * 8,
            frequency(output.sample_rate_hz)
        )
    };
    (badge, reason, is_perfect)
}

pub(crate) struct VerificationLine {
    pub(crate) text: String,
    pub(crate) evidence: String,
    pub(crate) is_verified: bool,
}

pub(crate) fn transmission(status: &TransmissionStatus, sample_rate_hz: u32) -> VerificationLine {
    let mut evidence = String::new();
    let text = match status {
        TransmissionStatus::InProgress { .. } => "Transmission : vérification en cours…".into(),
        TransmissionStatus::Verified {
            first_frame_index,
            md5,
            ..
        } => {
            evidence = digest(md5);
            if *first_frame_index == 0 {
                "Transmission vérifiée".into()
            } else {
                format!(
                    "Transmission vérifiée depuis {}",
                    clock(first_frame_index / u64::from(sample_rate_hz.max(1)))
                )
            }
        }
        TransmissionStatus::Mismatch {
            decoder_md5,
            device_md5,
            ..
        } => {
            evidence = format!(
                "Décodé : {} · Envoyé : {}",
                digest(decoder_md5),
                digest(device_md5)
            );
            "Transmission : écart entre les échantillons décodés et ceux envoyés au périphérique"
                .into()
        }
        TransmissionStatus::NotApplicableInSharedMode => {
            "Transmission : non applicable en mode partagé".into()
        }
        TransmissionStatus::NotVerifiable { reason } => format!(
            "Transmission : non vérifiable ({})",
            match reason {
                TransmissionNotVerifiableReason::StoppedBeforeTrackEnd =>
                    "lecture arrêtée avant la fin",
                TransmissionNotVerifiableReason::VerificationTapOverflowed =>
                    "mémoire de vérification saturée",
                TransmissionNotVerifiableReason::PartlyPlayedThroughSystemMixer =>
                    "une partie jouée en mode partagé",
                TransmissionNotVerifiableReason::QueuedAudioLostOnPause => "audio en attente perdu",
                TransmissionNotVerifiableReason::SegmentsNotAligned =>
                    "séquence de trames incohérente",
                TransmissionNotVerifiableReason::EmptySegment =>
                    "rien à vérifier après le déplacement",
            }
        ),
    };
    VerificationLine {
        text,
        evidence,
        is_verified: matches!(status, TransmissionStatus::Verified { .. }),
    }
}

pub(crate) fn integrity(status: &FileIntegrityStatus) -> VerificationLine {
    let mut evidence = String::new();
    let text = match status {
        FileIntegrityStatus::InProgress { .. } => "Fichier : vérification en cours…".into(),
        FileIntegrityStatus::Verified { md5 } => {
            evidence = digest(md5);
            "Fichier intègre (MD5 identique à la signature)".into()
        }
        FileIntegrityStatus::Mismatch {
            expected_md5,
            computed_md5,
            ..
        } => {
            evidence = format!(
                "Attendu : {} · Calculé : {}",
                digest(expected_md5),
                digest(computed_md5)
            );
            "Fichier : ne correspond pas à sa signature MD5".into()
        }
        FileIntegrityStatus::NoEmbeddedSignature { computed_md5 } => {
            evidence = computed_md5.as_ref().map(digest).unwrap_or_default();
            "Fichier : pas de signature MD5 dans le fichier".into()
        }
        FileIntegrityStatus::NotVerifiable { reason } => format!(
            "Fichier : non vérifiable ({})",
            match reason {
                FileIntegrityNotVerifiableReason::PlaybackRepositioned => "lecture déplacée",
                FileIntegrityNotVerifiableReason::StoppedBeforeTrackEnd =>
                    "lecture arrêtée avant la fin",
                FileIntegrityNotVerifiableReason::DecodeErrorInTrack => "erreur de décodage",
            }
        ),
    };
    VerificationLine {
        text,
        evidence,
        is_verified: matches!(status, FileIntegrityStatus::Verified { .. }),
    }
}

pub(crate) fn failure(kind: PlaybackFailureKind) -> &'static str {
    match kind {
        PlaybackFailureKind::FileUnreadable => "Le fichier ne peut pas être lu.",
        PlaybackFailureKind::UnsupportedFile => "Ce format de fichier n’est pas pris en charge.",
        PlaybackFailureKind::CorruptFile => "Le fichier est endommagé.",
        PlaybackFailureKind::DeviceNotFound => "La sortie audio est introuvable.",
        PlaybackFailureKind::DeviceUnplugged => {
            "La sortie audio a été débranchée. Lecture pour recommencer."
        }
        PlaybackFailureKind::DeviceInUse => {
            "La sortie audio est occupée par une autre application."
        }
        PlaybackFailureKind::ExclusiveModeDisabled => {
            "Le mode exclusif est désactivé par le système."
        }
        PlaybackFailureKind::FormatRefusedByDevice => "Le périphérique refuse le format audio.",
        PlaybackFailureKind::DeviceStoppedResponding => "La sortie audio ne répond plus.",
        PlaybackFailureKind::AudioServiceNotRunning => "Le service audio du système est arrêté.",
        PlaybackFailureKind::Internal => "La lecture a rencontré une erreur interne.",
    }
}

/// The shared-mode volume as shown next to the slider: "0 dB", "−12,5 dB" or "Muet".
pub(crate) fn volume_label(volume: SharedModeVolume) -> String {
    if volume.is_muted {
        return "Muet".into();
    }
    if volume.is_unity() {
        return "0 dB".into();
    }
    format!(
        "−{} dB",
        format!("{:.1}", -volume.decibels).replace('.', ",")
    )
}

/// One line explaining where the volume is set, for the output page.
pub(crate) fn volume_description(volume: SharedModeVolume, is_exclusive_output: bool) -> String {
    if is_exclusive_output {
        "Volume : réglé sur le DAC. En mode exclusif, Pistolame n’applique aucun gain.".into()
    } else {
        format!(
            "Volume : {}, appliqué avant le mixeur du système (mode partagé).",
            volume_label(volume)
        )
    }
}

/// Five stages of the active pipeline; no claim about physical DAC output.
pub(crate) fn signal_steps(
    value: &SignalPathAssessment,
    volume: SharedModeVolume,
) -> Vec<(&'static str, String, bool)> {
    let shared = value.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer;
    vec![
        (
            "Source",
            if value.source_codec.is_lossy {
                format!("{} (avec perte)", source_description(value))
            } else {
                format!(
                    "{} · {}",
                    value.source_codec.display_name,
                    source_description(value)
                )
            },
            !value.source_codec.is_lossy,
        ),
        if value.source_codec.is_decoded_as_float {
            (
                "Décodage",
                "flottant, arrondi à 24 bits entiers".into(),
                false,
            )
        } else {
            ("Décodage", "PCM entier, sans perte".into(), true)
        },
        if shared {
            (
                "Volume",
                format!("{} · avant le mixeur", volume_label(volume)),
                volume.is_unity(),
            )
        } else {
            ("Volume", "0 dB · réglé sur le DAC".into(), true)
        },
        (
            "Sortie",
            if shared {
                format!(
                    "{} partagé · mixeur du système",
                    value.negotiated.backend_name
                )
            } else if value.negotiated.sample_delivery
                == DeviceSampleDelivery::ExactFloat32Conversion
            {
                format!(
                    "{} exclusif · flottants 32 bits exacts",
                    value.negotiated.backend_name
                )
            } else {
                format!("{} exclusif", value.negotiated.backend_name)
            },
            !shared,
        ),
        (
            "DAC",
            value.negotiated.device.display_name.clone(),
            !shared && value.negotiated.sample_delivery == DeviceSampleDelivery::Unchanged,
        ),
    ]
}

/// The facts shown under "Informations sur le fichier" before any analysis: what the library
/// knows, plus the file size from the file system (`None` if it cannot be read).
pub(crate) fn track_info_cells(
    track: &pistolame_library::MusicFile,
    file_size_bytes: Option<u64>,
) -> Vec<(String, String)> {
    /// Bits per byte, for the average bitrate.
    const BITS_PER_BYTE: f64 = 8.0;
    let mut cells = vec![(
        "Format".to_owned(),
        format!(
            "{} ({})",
            track.codec,
            if track.is_lossy {
                "avec perte"
            } else {
                "sans perte"
            }
        ),
    )];
    if track.bits_per_sample > 0 {
        cells.push((
            "Résolution".to_owned(),
            format!("{} bits", track.bits_per_sample),
        ));
    }
    cells.extend([
        (
            "Fréquence".to_owned(),
            format!("{} kHz", frequency(track.sample_rate_hz)),
        ),
        ("Durée".to_owned(), clock(track.duration_seconds)),
    ]);
    if let Some(size) = file_size_bytes {
        if track.duration_seconds > 0 {
            let kilobits_per_second =
                size as f64 * BITS_PER_BYTE / 1000.0 / track.duration_seconds as f64;
            cells.push((
                "Débit moyen".to_owned(),
                format!("{} kb/s", kilobits_per_second.round() as u64),
            ));
        }
        cells.push((
            "Taille".to_owned(),
            format!(
                "{} Mo",
                format!("{:.1}", size as f64 / 1_000_000.0).replace('.', ",")
            ),
        ));
    }
    if track.track_number > 0 {
        cells.push((
            "Piste".to_owned(),
            if track.disc_number > 1 {
                format!("{} (disque {})", track.track_number, track.disc_number)
            } else {
                track.track_number.to_string()
            },
        ));
    }
    if let Some(year) = track.year {
        cells.push(("Année".to_owned(), year.to_string()));
    }
    if let Some(genre) = track.genre.as_ref().filter(|genre| !genre.is_empty()) {
        cells.push(("Genre".to_owned(), genre.clone()));
    }
    cells.push(("Lectures".to_owned(), track.play_count.to_string()));
    cells
}

/// What the output device list shows: a "Sortie du système" row naming the current system
/// default, then every device, with the user's choice checked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeviceListView {
    /// Display name of each device, in the engine's order.
    pub(crate) device_names: Vec<String>,
    /// Index in `device_names` of the checked device; -1 when "Sortie du système" is checked.
    pub(crate) selected_device_index: i32,
    /// Whether "Sortie du système" is checked.
    pub(crate) is_system_output_selected: bool,
    /// The device the system output plays to now; empty when the system has no default.
    pub(crate) system_output_device_name: String,
}

/// A specific choice checks the device the engine resolved it to (the system default while the
/// chosen device is absent, as the engine plays there).
pub(crate) fn device_list_view(
    devices: &[OutputDeviceInfo],
    selection: &OutputDeviceSelection,
    selected: Option<&OutputDeviceId>,
) -> DeviceListView {
    let is_system_output_selected = *selection == OutputDeviceSelection::SystemDefault;
    let selected_device_index = if is_system_output_selected {
        None
    } else {
        devices
            .iter()
            .position(|device| Some(&device.id) == selected)
    };
    DeviceListView {
        device_names: devices
            .iter()
            .map(|device| device.display_name.clone())
            .collect(),
        selected_device_index: selected_device_index
            .and_then(|index| i32::try_from(index).ok())
            .unwrap_or(-1),
        is_system_output_selected,
        system_output_device_name: devices
            .iter()
            .find(|device| device.is_system_default)
            .map(|device| device.display_name.clone())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speakers_and_headphones() -> Vec<OutputDeviceInfo> {
        vec![
            OutputDeviceInfo {
                id: OutputDeviceId("BuiltInSpeakerDevice".into()),
                display_name: "Haut-parleurs MacBook Air".into(),
                is_system_default: false,
            },
            OutputDeviceInfo {
                id: OutputDeviceId("BuiltInHeadphoneOutputDevice".into()),
                display_name: "Écouteurs externes".into(),
                is_system_default: true,
            },
        ]
    }

    #[test]
    fn the_system_output_row_names_the_current_default() {
        let devices = speakers_and_headphones();
        let view = device_list_view(
            &devices,
            &OutputDeviceSelection::SystemDefault,
            Some(&devices[1].id),
        );
        assert_eq!(
            view,
            DeviceListView {
                device_names: vec![
                    "Haut-parleurs MacBook Air".into(),
                    "Écouteurs externes".into()
                ],
                selected_device_index: -1,
                is_system_output_selected: true,
                system_output_device_name: "Écouteurs externes".into(),
            }
        );
    }

    #[test]
    fn a_specific_choice_checks_its_device() {
        let devices = speakers_and_headphones();
        let view = device_list_view(
            &devices,
            &OutputDeviceSelection::Specific(devices[0].id.clone()),
            Some(&devices[0].id),
        );
        assert!(!view.is_system_output_selected);
        assert_eq!(view.selected_device_index, 0);
        assert_eq!(view.system_output_device_name, "Écouteurs externes");
    }

    #[test]
    fn no_default_leaves_the_system_output_name_empty() {
        let mut devices = speakers_and_headphones();
        devices[1].is_system_default = false;
        let view = device_list_view(&devices, &OutputDeviceSelection::SystemDefault, None);
        assert_eq!(view.system_output_device_name, "");
    }
    #[test]
    fn missing_signature_never_means_verified() {
        let line = integrity(&FileIntegrityStatus::NoEmbeddedSignature { computed_md5: None });
        assert!(!line.is_verified);
        assert!(line.text.contains("pas de signature"));
    }
    #[test]
    fn verification_after_seek_names_the_actual_start() {
        let line = transmission(
            &TransmissionStatus::Verified {
                first_frame_index: 83 * 44_100,
                frame_count: 44_100,
                md5: [0; 16],
            },
            44_100,
        );
        assert_eq!(line.text, "Transmission vérifiée depuis 1:23");
        assert!(line.is_verified);
    }
    #[test]
    fn shared_mode_is_never_verified() {
        assert!(!transmission(&TransmissionStatus::NotApplicableInSharedMode, 44_100).is_verified);
    }
    #[test]
    fn volume_labels_use_decibels_and_a_true_minus_sign() {
        assert_eq!(volume_label(SharedModeVolume::default()), "0 dB");
        assert_eq!(
            volume_label(SharedModeVolume::at_decibels(-12.5)),
            "−12,5 dB"
        );
        let muted = SharedModeVolume {
            is_muted: true,
            ..SharedModeVolume::default()
        };
        assert_eq!(volume_label(muted), "Muet");
    }
    #[test]
    fn audio_formats_name_bits_and_kilohertz() {
        assert_eq!(audio_format(16, 44_100), "16 bits / 44,1 kHz");
        assert_eq!(audio_format(24, 192_000), "24 bits / 192 kHz");
    }
    #[test]
    fn formats_use_french_decimal_comma() {
        assert_eq!(frequency(44_100), "44,1");
        assert_eq!(clock(125), "2:05");
        assert_eq!(remaining_clock(125), "−2:05");
    }
    fn exclusive_assessment(delivery: DeviceSampleDelivery) -> SignalPathAssessment {
        let source = StreamFormat {
            sample_rate_hz: 96_000,
            bits_per_sample: 24,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        };
        let render_format = DeviceSampleFormat {
            sample_rate_hz: 96_000,
            container_bytes: 4,
            valid_bits_per_sample: 24,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        };
        let negotiated = NegotiatedOutput {
            backend_name: "CoreAudio",
            device: OutputDeviceInfo {
                id: OutputDeviceId("headphones".into()),
                display_name: "Écouteurs externes".into(),
                is_system_default: true,
            },
            access_mode: DeviceAccessMode::Exclusive,
            shared_mode_cause: None,
            device_format: Some(render_format),
            system_mix_format: None,
            render_format,
            sample_delivery: delivery,
            buffer_frame_count: 512,
            device_period_in_hundred_nanoseconds: 53_333,
            negotiation_log: Vec::new(),
        };
        assess_signal_path(
            &source,
            &pistolame_core::SourceCodec::FLAC,
            &negotiated,
            &ProcessingState::default(),
        )
    }
    #[test]
    fn exact_float_delivery_shows_exclusive_never_bit_perfect() {
        let (badge, reason, is_perfect) = assessment(&exclusive_assessment(
            DeviceSampleDelivery::ExactFloat32Conversion,
        ));
        assert_eq!(badge, "Exclusif · 24 bits / 96 kHz");
        assert!(!is_perfect);
        assert!(reason.contains("CoreAudio exclusif"));
        assert!(reason.contains("pas vérifiable"));
    }
    #[test]
    fn unchanged_exclusive_delivery_names_the_backend() {
        let (badge, reason, is_perfect) =
            assessment(&exclusive_assessment(DeviceSampleDelivery::Unchanged));
        assert_eq!(badge, "Bit-perfect · 24 bits / 96 kHz");
        assert!(is_perfect);
        assert!(reason.ends_with("CoreAudio exclusif"));
    }
    #[test]
    fn compact_format_uses_the_decimal_comma_and_names_lossy_codecs() {
        assert_eq!(compact_format(16, 44_100, None), "16/44,1");
        assert_eq!(compact_format(24, 192_000, None), "24/192");
        assert_eq!(compact_format(0, 44_100, Some("MP3")), "MP3");
    }
    fn identity<'album>(
        title: &'album str,
        artist: &'album str,
        format: &str,
        track_count: usize,
    ) -> AlbumIdentity<'album> {
        AlbumIdentity {
            title,
            artist,
            format: format.to_owned(),
            track_count,
        }
    }
    #[test]
    fn namesakes_are_told_apart_by_format_then_by_track_count() {
        let labels = distinguishing_labels(&[
            identity("LA VIE CONTINUE", "Maes", "24/48", 14),
            identity("La vie continue", "maes", "24/44,1", 14),
            identity("Fellowship", "Howard Shore", "24/48", 13),
            identity("Fellowship", "Howard Shore", "24/48", 1),
            identity("La vie continue", "Autre artiste", "16/44,1", 9),
        ]);
        assert_eq!(
            labels,
            [
                Some("24/48".to_owned()),
                Some("24/44,1".to_owned()),
                Some("13 titres".to_owned()),
                Some("1 titre".to_owned()),
                None,
            ]
        );
    }
    #[test]
    fn tile_subtitle_adds_the_format_only_when_given() {
        assert_eq!(album_tile_subtitle("Maes", Some("24/48")), "Maes · 24/48");
        assert_eq!(album_tile_subtitle("Maes", None), "Maes");
    }
}
