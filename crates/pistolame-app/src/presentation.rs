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

pub(crate) fn assessment(value: &SignalPathAssessment) -> (String, String, bool) {
    let source = value.source_format;
    let format = format!(
        "{}/{}",
        source.bits_per_sample,
        frequency(source.sample_rate_hz)
    );
    let is_perfect = value.verdict == BitPerfectVerdict::BitPerfect;
    let shared = value.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer;
    let badge = format!(
        "{} · {format}",
        if is_perfect {
            "Bit-perfect"
        } else if shared {
            "Mode partagé"
        } else {
            "Traité"
        }
    );
    let reason = if shared {
        let cause = match value.negotiated.shared_mode_cause {
            Some(SharedModeCause::ExclusiveModeDisabledBySystem) => {
                "Mode exclusif désactivé dans Windows. "
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
            "{cause}Windows peut convertir, rééchantillonner ou mixer le signal ; le bit-perfect n’est pas garanti.{mix}"
        )
    } else {
        let output = value.negotiated.render_format;
        format!(
            "{} bits / {} kHz → {} bits dans {} / {} kHz · WASAPI exclusif",
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
            "Le mode exclusif est désactivé dans Windows."
        }
        PlaybackFailureKind::FormatRefusedByDevice => "Le périphérique refuse le format audio.",
        PlaybackFailureKind::DeviceStoppedResponding => "La sortie audio ne répond plus.",
        PlaybackFailureKind::AudioServiceNotRunning => "Le service audio Windows est arrêté.",
        PlaybackFailureKind::Internal => "La lecture a rencontré une erreur interne.",
    }
}

/// Five stages of the active pipeline; no claim about physical DAC output.
pub(crate) fn signal_steps(value: &SignalPathAssessment) -> Vec<(&'static str, String, bool)> {
    let source = value.source_format;
    let shared = value.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer;
    vec![
        (
            "Source",
            format!(
                "FLAC · {} bits / {} kHz",
                source.bits_per_sample,
                frequency(source.sample_rate_hz)
            ),
            true,
        ),
        ("Décodage", "PCM entier, sans perte".into(), true),
        ("Volume", "100 % · aucun gain logiciel".into(), true),
        (
            "Sortie",
            if shared {
                "WASAPI partagé · mixeur Windows".into()
            } else {
                "WASAPI exclusif".into()
            },
            !shared,
        ),
        ("DAC", value.negotiated.device.display_name.clone(), !shared),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn formats_use_french_decimal_comma() {
        assert_eq!(frequency(44_100), "44,1");
        assert_eq!(clock(125), "2:05");
        assert_eq!(remaining_clock(125), "−2:05");
    }
}
