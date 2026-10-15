//! The playback devices ALSA knows, and which PCM name opens each one in each mode.
//!
//! Device identifiers are `hw:CARD=<card id>,DEV=<n>`: the card id (for example `K11`) stays the
//! same across reboots and USB ports, unlike the card number. The system default is ALSA's
//! `default` PCM, which desktop systems route to their sound server (PipeWire or PulseAudio).

use alsa::Direction;
use alsa::card::Card;
use alsa::ctl::{Ctl, DeviceIter};

use crate::{OutputDeviceId, OutputDeviceInfo, OutputError};

/// ALSA's default PCM: the sound server, which mixes and converts like a system mixer.
pub(super) const SYSTEM_DEFAULT_PCM_NAME: &str = "default";
/// Prefix of a direct hardware PCM (no conversion, no mixing).
const HARDWARE_PCM_PREFIX: &str = "hw:";
/// Prefix of the same hardware behind ALSA's `plug` converter (shared mode on a chosen card).
const CONVERTING_PCM_PREFIX: &str = "plughw:";
/// Shown for the `default` PCM in the device list.
const SYSTEM_DEFAULT_DISPLAY_NAME: &str = "Sortie par défaut (serveur de son)";

/// Which PCM opens a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DeviceRoute {
    /// The `default` PCM: only shared mode exists there.
    SystemDefault,
    /// A sound card: `hw:` for exclusive mode, `plughw:` for shared mode.
    Hardware {
        exclusive_pcm_name: String,
        shared_pcm_name: String,
    },
}

impl DeviceRoute {
    /// The route of `device_id`, or `None` for an identifier this backend never produced.
    pub(super) fn of(device_id: &OutputDeviceId) -> Option<DeviceRoute> {
        let id = device_id.0.as_str();
        if id == SYSTEM_DEFAULT_PCM_NAME {
            return Some(DeviceRoute::SystemDefault);
        }
        let card_and_device = id.strip_prefix(HARDWARE_PCM_PREFIX)?;
        // A NUL byte could never reach ALSA's C API; refuse it here, before any CString.
        if card_and_device.is_empty() || card_and_device.contains('\0') {
            return None;
        }
        Some(DeviceRoute::Hardware {
            exclusive_pcm_name: id.to_owned(),
            shared_pcm_name: format!("{CONVERTING_PCM_PREFIX}{card_and_device}"),
        })
    }

    /// The PCM to open in shared mode.
    pub(super) fn shared_pcm_name(&self) -> &str {
        match self {
            DeviceRoute::SystemDefault => SYSTEM_DEFAULT_PCM_NAME,
            DeviceRoute::Hardware {
                shared_pcm_name, ..
            } => shared_pcm_name,
        }
    }
}

/// The system default first, then every playback device of every sound card. A card that
/// cannot be read is skipped (it is listed again once it answers).
pub(super) fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, OutputError> {
    let mut devices = vec![OutputDeviceInfo {
        id: OutputDeviceId(SYSTEM_DEFAULT_PCM_NAME.to_owned()),
        display_name: SYSTEM_DEFAULT_DISPLAY_NAME.to_owned(),
        is_system_default: true,
    }];
    for card in alsa::card::Iter::new().flatten() {
        devices.extend(card_playback_devices(&card));
    }
    Ok(devices)
}

/// The playback devices of one card.
fn card_playback_devices(card: &Card) -> Vec<OutputDeviceInfo> {
    let Ok(control) = Ctl::from_card(card, false) else {
        return Vec::new();
    };
    let Ok(card_info) = control.card_info() else {
        return Vec::new();
    };
    let (Ok(card_id), Ok(card_name)) = (card_info.get_id(), card_info.get_name()) else {
        return Vec::new();
    };
    DeviceIter::new(&control)
        .filter_map(|device_number| {
            let device_number = u32::try_from(device_number).ok()?;
            // Only devices with a playback stream answer this query.
            let pcm_info = control
                .pcm_info(device_number, 0, Direction::Playback)
                .ok()?;
            let pcm_name = pcm_info.get_name().unwrap_or_default();
            Some(OutputDeviceInfo {
                id: OutputDeviceId(format!(
                    "{HARDWARE_PCM_PREFIX}CARD={card_id},DEV={device_number}"
                )),
                display_name: display_name(card_name, pcm_name),
                is_system_default: false,
            })
        })
        .collect()
}

/// "FiiO K11 — USB Audio", or the card name alone when the device has no name of its own.
fn display_name(card_name: &str, pcm_name: &str) -> String {
    if pcm_name.is_empty() || pcm_name == card_name {
        card_name.to_owned()
    } else {
        format!("{card_name} — {pcm_name}")
    }
}

/// Whether `device_id` is still listed (unplugged USB DAC: no longer).
pub(super) fn is_device_listed(device_id: &OutputDeviceId) -> bool {
    list_output_devices().is_ok_and(|devices| devices.iter().any(|device| &device.id == device_id))
}

/// `DeviceNotFound` unless `device_id` is listed. ALSA would open a missing `hw:` name with a
/// less helpful error.
pub(super) fn ensure_listed(device_id: &OutputDeviceId) -> Result<DeviceRoute, OutputError> {
    let route = DeviceRoute::of(device_id).ok_or(OutputError::DeviceNotFound)?;
    if route != DeviceRoute::SystemDefault && !is_device_listed(device_id) {
        return Err(OutputError::DeviceNotFound);
    }
    Ok(route)
}

/// The listed information of `device_id` (the default PCM is always listed).
pub(super) fn device_info(device_id: &OutputDeviceId) -> Result<OutputDeviceInfo, OutputError> {
    list_output_devices()?
        .into_iter()
        .find(|device| &device.id == device_id)
        .ok_or(OutputError::DeviceNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> OutputDeviceId {
        OutputDeviceId(text.to_owned())
    }

    #[test]
    fn a_card_opens_hw_exclusively_and_plughw_shared() {
        let route = DeviceRoute::of(&id("hw:CARD=K11,DEV=0")).expect("route");
        assert_eq!(
            route,
            DeviceRoute::Hardware {
                exclusive_pcm_name: "hw:CARD=K11,DEV=0".into(),
                shared_pcm_name: "plughw:CARD=K11,DEV=0".into(),
            }
        );
        assert_eq!(route.shared_pcm_name(), "plughw:CARD=K11,DEV=0");
    }

    #[test]
    fn the_default_pcm_is_the_system_default() {
        let route = DeviceRoute::of(&id("default")).expect("route");
        assert_eq!(route, DeviceRoute::SystemDefault);
        assert_eq!(route.shared_pcm_name(), "default");
    }

    #[test]
    fn foreign_identifiers_have_no_route() {
        assert_eq!(DeviceRoute::of(&id("{0.0.0.00000000}.{abc}")), None);
        assert_eq!(DeviceRoute::of(&id("hw:")), None);
        assert_eq!(DeviceRoute::of(&id("hw:CARD=A\0B")), None);
    }

    #[test]
    fn names_skip_a_repeated_card_name() {
        assert_eq!(
            display_name("FiiO K11", "USB Audio"),
            "FiiO K11 — USB Audio"
        );
        assert_eq!(display_name("FiiO K11", ""), "FiiO K11");
        assert_eq!(display_name("HDA Intel", "HDA Intel"), "HDA Intel");
    }
}
