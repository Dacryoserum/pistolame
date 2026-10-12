//! The shared-mode volume: an attenuation in decibels applied by the platform's mixer stage,
//! never in exclusive mode, where the device receives the samples unchanged (bit-perfect) and
//! the volume is set on the DAC.

/// Quietest audible setting; one step below it is mute. 60 dB of attenuation is where foobar2000
/// and most players stop being useful before muting.
pub const MINIMUM_SHARED_VOLUME_DECIBELS: f32 = -60.0;
/// Loudest setting. No gain above 0 dB: it would clip loudly mastered tracks.
pub const MAXIMUM_SHARED_VOLUME_DECIBELS: f32 = 0.0;

/// The volume the user chose for shared mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SharedModeVolume {
    /// Attenuation, clamped to `MINIMUM_SHARED_VOLUME_DECIBELS..=MAXIMUM_SHARED_VOLUME_DECIBELS`.
    pub decibels: f32,
    /// Silence, whatever `decibels` says.
    pub is_muted: bool,
}

impl Default for SharedModeVolume {
    /// Full level: nothing is attenuated until the user asks.
    fn default() -> Self {
        SharedModeVolume {
            decibels: MAXIMUM_SHARED_VOLUME_DECIBELS,
            is_muted: false,
        }
    }
}

impl SharedModeVolume {
    /// A volume at `decibels`, clamped to the accepted range.
    pub fn at_decibels(decibels: f32) -> SharedModeVolume {
        SharedModeVolume {
            decibels: decibels.clamp(
                MINIMUM_SHARED_VOLUME_DECIBELS,
                MAXIMUM_SHARED_VOLUME_DECIBELS,
            ),
            is_muted: false,
        }
    }

    /// Amplitude factor for the samples: 10^(dB / 20), or 0 when muted.
    pub fn linear_gain(self) -> f32 {
        if self.is_muted || !self.decibels.is_finite() {
            return 0.0;
        }
        10.0_f32.powf(
            self.decibels.clamp(
                MINIMUM_SHARED_VOLUME_DECIBELS,
                MAXIMUM_SHARED_VOLUME_DECIBELS,
            ) / 20.0,
        )
    }

    /// Whether the samples leave the mixer stage unchanged.
    pub fn is_unity(self) -> bool {
        !self.is_muted && self.decibels >= MAXIMUM_SHARED_VOLUME_DECIBELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_level_is_unity_gain() {
        assert_eq!(SharedModeVolume::default().linear_gain(), 1.0);
        assert!(SharedModeVolume::default().is_unity());
    }

    #[test]
    fn decibels_map_to_amplitude() {
        let minus_six = SharedModeVolume::at_decibels(-6.0).linear_gain();
        assert!((minus_six - 0.501_187).abs() < 1e-5);
        let minus_twenty = SharedModeVolume::at_decibels(-20.0).linear_gain();
        assert!((minus_twenty - 0.1).abs() < 1e-6);
    }

    #[test]
    fn values_are_clamped_and_mute_is_silence() {
        assert_eq!(SharedModeVolume::at_decibels(6.0).decibels, 0.0);
        assert_eq!(SharedModeVolume::at_decibels(-90.0).decibels, -60.0);
        let muted = SharedModeVolume {
            is_muted: true,
            ..SharedModeVolume::default()
        };
        assert_eq!(muted.linear_gain(), 0.0);
        assert!(!muted.is_unity());
    }
}
