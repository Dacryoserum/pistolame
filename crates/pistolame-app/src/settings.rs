//! The settings file: the few choices the app remembers between runs (task T15).
//!
//! The file is plain UTF-8 text, one `key = value` per line, `#` starts a comment. Reading never
//! fails and never blocks start-up: anything unusable falls back to the default for that key
//! only. Writing is atomic so a crash cannot leave a half-written file.
//!
//! Output device: `output_device_id` is written only for a specific device; its absence means
//! "Sortie du système" (follow the system default). Files from older versions migrate without
//! a rewrite: no `output_device_id` line (never picked a device) gives the new default, "Sortie
//! du système"; a stored id stays that specific device, because it was an explicit pick in the
//! device list and may be a DAC the user wants to keep whatever the system default is.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use pistolame_core::{
    EngineStartOptions, OutputDeviceId, OutputDeviceSelection, OutputModePreference,
    SharedModeVolume,
};

const OUTPUT_DEVICE_ID_KEY: &str = "output_device_id";
const OUTPUT_DEVICE_NAME_KEY: &str = "output_device_name";
const OUTPUT_MODE_KEY: &str = "output_mode";
const EXCLUSIVE_MODE_NOTICE_SEEN_KEY: &str = "exclusive_mode_notice_seen";
const SHARED_VOLUME_DECIBELS_KEY: &str = "shared_volume_db";
const SHARED_VOLUME_MUTED_KEY: &str = "shared_volume_muted";
const ALBUM_SORT_KEY: &str = "album_sort";
const ARTIST_PHOTOS_KEY: &str = "artist_photos";
const START_SECTION_KEY: &str = "start_section";
const SINGLE_CLICK_PLAY_KEY: &str = "single_click_play";
const TRACK_SORT_KEY: &str = "track_sort";
const TRACKS_FAVORITES_ONLY_KEY: &str = "tracks_favorites_only";
const WINDOW_GEOMETRY_KEY: &str = "window_geometry";
/// Words after the column number of `track_sort`.
const ASCENDING_VALUE: &str = "ascending";
const DESCENDING_VALUE: &str = "descending";
/// Volume is stored in tenths of a decibel, so the settings stay exact integers.
const TENTHS_PER_DECIBEL: f32 = 10.0;

const SHARED_MODE_VALUE: &str = "shared";
const EXCLUSIVE_MODE_VALUE: &str = "exclusive";
const TRUE_VALUE: &str = "true";
const FALSE_VALUE: &str = "false";

const COMMENT_PREFIX: char = '#';
const KEY_VALUE_SEPARATOR: char = '=';

const SETTINGS_FILE_NAME: &str = "settings.txt";
const TEMPORARY_FILE_NAME: &str = "settings.txt.tmp";
/// SQLite file of the persistent library (docs/plans/persistent-library.md).
const LIBRARY_FILE_NAME: &str = "library.sqlite3";
/// The app's folder inside the system configuration folder: capitalized product name on Windows
/// and macOS, lowercase on Linux (each platform's convention).
const APP_FOLDER_NAME: &str = if cfg!(target_os = "linux") {
    "pistolame"
} else {
    "Pistolame"
};
/// The folder older versions used on macOS, under `~/.config`.
const LEGACY_FOLDER_NAME: &str = "pistolame";
const HOME_CONFIG_FOLDER_NAME: &str = ".config";

/// Choices remembered between runs.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AppSettings {
    /// The output device the user chose: "Sortie du système" (the default) or a device.
    pub output_device: OutputDeviceSelection,
    /// Display name of a specific device, kept so the app can say which device went missing.
    pub output_device_name: Option<String>,
    /// Shared (default) or exclusive access mode.
    pub output_mode: OutputModePreference,
    /// Whether the one-time exclusive-mode notice was already shown.
    pub exclusive_mode_notice_seen: bool,
    /// Shared-mode volume in tenths of a decibel (0 = full level, -600 = -60 dB).
    pub shared_volume_tenths_of_decibel: i32,
    /// Whether the shared-mode volume is muted.
    pub is_shared_volume_muted: bool,
    /// Order of the album grid (`AlbumSort::key`), `None` for the default.
    pub album_sort: Option<String>,
    /// Whether the user turned off artist photos (looked up online by artist name; on by
    /// default).
    pub are_artist_photos_off: bool,
    /// Last library section shown (`SidebarSection::settings_key`), `None` for the default.
    pub start_section: Option<String>,
    /// A single click plays a track (otherwise it selects it and a double click plays).
    pub is_single_click_play: bool,
    /// Order of the "Morceaux" table: column number and whether it is descending.
    pub track_sort: Option<(i32, bool)>,
    /// Whether the "Morceaux" table shows only the favorites.
    pub is_tracks_favorites_only: bool,
    /// Window position and size at the last exit, to reopen it in the same place.
    pub window_geometry: Option<WindowGeometry>,
    /// First problem met while loading, for diagnostics only. Never shown to the user.
    pub load_problem: Option<String>,
}

/// Position and size of the main window, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowGeometry {
    fn serialize(&self) -> String {
        format!("{},{},{},{}", self.x, self.y, self.width, self.height)
    }

    fn parse(value: &str) -> Option<WindowGeometry> {
        let mut numbers = value.split(',').map(|part| part.trim().parse::<i32>());
        let geometry = WindowGeometry {
            x: numbers.next()?.ok()?,
            y: numbers.next()?.ok()?,
            width: numbers.next()?.ok()?,
            height: numbers.next()?.ok()?,
        };
        (numbers.next().is_none() && geometry.width > 0 && geometry.height > 0).then_some(geometry)
    }
}

impl AppSettings {
    /// Reads the settings file. Never fails: a missing, unreadable or invalid file gives the
    /// defaults, key by key, so start-up is never blocked by this file.
    pub fn load() -> AppSettings {
        match settings_file_path() {
            Some(path) => AppSettings::load_from(&path),
            None => AppSettings {
                load_problem: Some("no settings directory could be determined".to_owned()),
                ..AppSettings::default()
            },
        }
    }

    /// Writes the settings file atomically (temporary file, then rename).
    pub fn save(&self) -> anyhow::Result<()> {
        let path = settings_file_path().context("no settings directory could be determined")?;
        self.save_to(&path)
    }

    /// The start-up choices the engine needs; the engine itself never touches the file system.
    pub fn engine_start_options(&self) -> EngineStartOptions {
        EngineStartOptions {
            output_mode_preference: self.output_mode,
            device_selection: self.output_device.clone(),
            shared_mode_volume: self.shared_mode_volume(),
        }
    }

    /// The remembered shared-mode volume.
    pub fn shared_mode_volume(&self) -> SharedModeVolume {
        SharedModeVolume {
            is_muted: self.is_shared_volume_muted,
            ..SharedModeVolume::at_decibels(
                self.shared_volume_tenths_of_decibel as f32 / TENTHS_PER_DECIBEL,
            )
        }
    }

    /// Remembers `volume` (saved by the caller).
    pub fn set_shared_mode_volume(&mut self, volume: SharedModeVolume) {
        self.shared_volume_tenths_of_decibel =
            (volume.decibels * TENTHS_PER_DECIBEL).round() as i32;
        self.is_shared_volume_muted = volume.is_muted;
    }

    /// Like [`AppSettings::load`], from an explicit path (lets tests avoid the real directory).
    fn load_from(path: &Path) -> AppSettings {
        match fs::read(path) {
            Ok(bytes) => AppSettings::parse(&String::from_utf8_lossy(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppSettings::default(),
            Err(error) => AppSettings {
                load_problem: Some(format!("cannot read {}: {error}", path.display())),
                ..AppSettings::default()
            },
        }
    }

    /// Like [`AppSettings::save`], to an explicit path.
    fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let directory = path
            .parent()
            .context("settings path has no parent directory")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        let temporary_path = directory.join(TEMPORARY_FILE_NAME);
        self.write_temporary_file(&temporary_path)?;
        fs::rename(&temporary_path, path).with_context(|| {
            format!(
                "cannot move {} to {}",
                temporary_path.display(),
                path.display()
            )
        })
    }

    fn write_temporary_file(&self, temporary_path: &Path) -> anyhow::Result<()> {
        let mut file = fs::File::create(temporary_path)
            .with_context(|| format!("cannot create {}", temporary_path.display()))?;
        file.write_all(self.serialize()?.as_bytes())
            .with_context(|| format!("cannot write {}", temporary_path.display()))?;
        // Flush to disk before the rename so the renamed file is never empty after a crash.
        file.sync_all()
            .with_context(|| format!("cannot flush {}", temporary_path.display()))
    }

    /// Builds the file text. Fails if a value could not be read back (it holds a line break).
    fn serialize(&self) -> anyhow::Result<String> {
        let mut text =
            String::from("# Pistolame settings. Edited by the app; unknown keys are ignored.\n");
        if let OutputDeviceSelection::Specific(device_id) = &self.output_device {
            push_entry(&mut text, OUTPUT_DEVICE_ID_KEY, &device_id.0)?;
            if let Some(device_name) = &self.output_device_name {
                push_entry(&mut text, OUTPUT_DEVICE_NAME_KEY, device_name)?;
            }
        }
        let mode_value = match self.output_mode {
            OutputModePreference::Shared => SHARED_MODE_VALUE,
            OutputModePreference::Exclusive => EXCLUSIVE_MODE_VALUE,
        };
        push_entry(&mut text, OUTPUT_MODE_KEY, mode_value)?;
        let notice_value = if self.exclusive_mode_notice_seen {
            TRUE_VALUE
        } else {
            FALSE_VALUE
        };
        push_entry(&mut text, EXCLUSIVE_MODE_NOTICE_SEEN_KEY, notice_value)?;
        push_entry(
            &mut text,
            SHARED_VOLUME_DECIBELS_KEY,
            &format!(
                "{:.1}",
                self.shared_volume_tenths_of_decibel as f32 / TENTHS_PER_DECIBEL
            ),
        )?;
        push_entry(
            &mut text,
            SHARED_VOLUME_MUTED_KEY,
            if self.is_shared_volume_muted {
                TRUE_VALUE
            } else {
                FALSE_VALUE
            },
        )?;
        if let Some(sort) = &self.album_sort {
            push_entry(&mut text, ALBUM_SORT_KEY, sort)?;
        }
        if self.are_artist_photos_off {
            push_entry(&mut text, ARTIST_PHOTOS_KEY, FALSE_VALUE)?;
        }
        if let Some(section) = &self.start_section {
            push_entry(&mut text, START_SECTION_KEY, section)?;
        }
        if self.is_single_click_play {
            push_entry(&mut text, SINGLE_CLICK_PLAY_KEY, TRUE_VALUE)?;
        }
        if let Some((column, is_descending)) = self.track_sort {
            let direction = if is_descending {
                DESCENDING_VALUE
            } else {
                ASCENDING_VALUE
            };
            push_entry(&mut text, TRACK_SORT_KEY, &format!("{column} {direction}"))?;
        }
        if self.is_tracks_favorites_only {
            push_entry(&mut text, TRACKS_FAVORITES_ONLY_KEY, TRUE_VALUE)?;
        }
        if let Some(geometry) = &self.window_geometry {
            push_entry(&mut text, WINDOW_GEOMETRY_KEY, &geometry.serialize())?;
        }
        Ok(text)
    }

    /// Parses file text leniently; the first problem found is kept in `load_problem`.
    fn parse(text: &str) -> AppSettings {
        let mut settings = AppSettings::default();
        let mut first_problem: Option<String> = None;
        let mut note_problem = |message: String| {
            first_problem.get_or_insert(message);
        };

        let mut entries: HashMap<&str, &str> = HashMap::new();
        for (index, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with(COMMENT_PREFIX) {
                continue;
            }
            match line.split_once(KEY_VALUE_SEPARATOR) {
                Some((key, value)) => {
                    entries.insert(key.trim(), value.trim());
                }
                None => note_problem(format!("line {}: expected `key = value`", index + 1)),
            }
        }

        if let Some(value) = entries.get(OUTPUT_DEVICE_ID_KEY).filter(|v| !v.is_empty()) {
            settings.output_device =
                OutputDeviceSelection::Specific(OutputDeviceId((*value).to_owned()));
        }
        if let Some(value) = entries
            .get(OUTPUT_DEVICE_NAME_KEY)
            .filter(|v| !v.is_empty())
        {
            settings.output_device_name = Some((*value).to_owned());
        }
        if let Some(value) = entries.get(OUTPUT_MODE_KEY) {
            match *value {
                SHARED_MODE_VALUE => settings.output_mode = OutputModePreference::Shared,
                EXCLUSIVE_MODE_VALUE => settings.output_mode = OutputModePreference::Exclusive,
                other => note_problem(format!("{OUTPUT_MODE_KEY}: invalid value `{other}`")),
            }
        }
        if let Some(value) = entries.get(EXCLUSIVE_MODE_NOTICE_SEEN_KEY) {
            match *value {
                TRUE_VALUE => settings.exclusive_mode_notice_seen = true,
                FALSE_VALUE => settings.exclusive_mode_notice_seen = false,
                other => note_problem(format!(
                    "{EXCLUSIVE_MODE_NOTICE_SEEN_KEY}: invalid value `{other}`"
                )),
            }
        }

        if let Some(value) = entries.get(SHARED_VOLUME_DECIBELS_KEY) {
            match value.parse::<f32>() {
                Ok(decibels) => {
                    settings.set_shared_mode_volume(SharedModeVolume::at_decibels(decibels))
                }
                Err(_) => note_problem(format!(
                    "{SHARED_VOLUME_DECIBELS_KEY}: invalid value `{value}`"
                )),
            }
        }
        if let Some(value) = entries.get(SHARED_VOLUME_MUTED_KEY) {
            settings.is_shared_volume_muted = *value == TRUE_VALUE;
        }

        if let Some(value) = entries
            .get(ALBUM_SORT_KEY)
            .filter(|value| !value.is_empty())
        {
            settings.album_sort = Some((*value).to_owned());
        }

        if let Some(value) = entries.get(ARTIST_PHOTOS_KEY) {
            settings.are_artist_photos_off = *value == FALSE_VALUE;
        }

        if let Some(value) = entries
            .get(START_SECTION_KEY)
            .filter(|value| !value.is_empty())
        {
            settings.start_section = Some((*value).to_owned());
        }
        settings.is_single_click_play = entries.get(SINGLE_CLICK_PLAY_KEY) == Some(&TRUE_VALUE);
        settings.track_sort = entries.get(TRACK_SORT_KEY).and_then(|value| {
            let (column, direction) = value.split_once(' ')?;
            let is_descending = match direction.trim() {
                DESCENDING_VALUE => true,
                ASCENDING_VALUE => false,
                _ => return None,
            };
            Some((column.trim().parse().ok()?, is_descending))
        });
        settings.is_tracks_favorites_only =
            entries.get(TRACKS_FAVORITES_ONLY_KEY) == Some(&TRUE_VALUE);
        settings.window_geometry = entries
            .get(WINDOW_GEOMETRY_KEY)
            .and_then(|value| WindowGeometry::parse(value));
        settings.load_problem = first_problem;
        settings
    }
}

fn push_entry(text: &mut String, key: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.contains(['\n', '\r']),
        "value of `{key}` contains a line break"
    );
    text.push_str(&format!("{key} {KEY_VALUE_SEPARATOR} {value}\n"));
    Ok(())
}

/// Location of the settings file: `%APPDATA%\Pistolame\settings.txt`, else
/// `$XDG_CONFIG_HOME/pistolame/settings.txt`, else `$HOME/.config/pistolame/settings.txt`.
/// Chosen from environment variables so no `cfg` is needed (plan T15).
fn settings_file_path() -> Option<PathBuf> {
    Some(app_data_directory()?.join(SETTINGS_FILE_NAME))
}

/// The library database, beside the settings file.
pub(crate) fn library_database_path() -> Option<PathBuf> {
    Some(app_data_directory()?.join(LIBRARY_FILE_NAME))
}

/// The folder holding everything the app stores, in each system's own convention:
/// `%APPDATA%\Pistolame` (Windows), `~/Library/Application Support/Pistolame` (macOS),
/// `$XDG_CONFIG_HOME/pistolame` or `~/.config/pistolame` (Linux).
fn app_data_directory() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join(APP_FOLDER_NAME))
}

/// Moves the settings and the library from the folder older versions used on macOS
/// (`~/.config/pistolame`) to the system's folder, once. Runs before anything is read.
/// Never overwrites: it does nothing when the new folder already exists. Best effort: on failure
/// the app starts with the new, empty folder and the old files stay where they were.
pub(crate) fn migrate_legacy_app_data() {
    let (Some(new_directory), Some(home)) = (app_data_directory(), dirs::home_dir()) else {
        return;
    };
    let legacy_directory = home.join(HOME_CONFIG_FOLDER_NAME).join(LEGACY_FOLDER_NAME);
    let _ = move_directory_once(&legacy_directory, &new_directory);
}

/// Moves `from` to `to` when `from` exists and `to` does not. Same-volume rename first, then
/// copy-and-delete (the user's home and its Library are normally on one volume).
fn move_directory_once(from: &Path, to: &Path) -> std::io::Result<bool> {
    if from == to || !from.is_dir() || to.exists() {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(true);
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    fs::remove_dir_all(from)?;
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Unique empty directory under the system temp directory, never the real settings folder.
    fn fresh_test_directory(test_name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "pistolame-settings-test-{}-{test_name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn missing_file_gives_defaults_without_problem() {
        let directory = fresh_test_directory("missing");
        let settings = AppSettings::load_from(&directory.join(SETTINGS_FILE_NAME));
        assert_eq!(settings, AppSettings::default());
        assert_eq!(settings.output_mode, OutputModePreference::Shared);
        assert_eq!(settings.output_device, OutputDeviceSelection::SystemDefault);
        assert!(!settings.exclusive_mode_notice_seen);
        assert_eq!(settings.load_problem, None);
    }

    #[test]
    fn save_then_load_round_trips() {
        let directory = fresh_test_directory("round-trip");
        let path = directory.join("nested").join(SETTINGS_FILE_NAME);
        let saved = AppSettings {
            output_device: OutputDeviceSelection::Specific(OutputDeviceId(
                "{0.0.0.00000000}.{abc}".to_owned(),
            )),
            output_device_name: Some("FiiO K11 (Haut-parleurs)".to_owned()),
            output_mode: OutputModePreference::Exclusive,
            exclusive_mode_notice_seen: true,
            shared_volume_tenths_of_decibel: -60,
            is_shared_volume_muted: false,
            album_sort: Some("year".to_owned()),
            are_artist_photos_off: true,
            start_section: Some("artists".to_owned()),
            is_single_click_play: true,
            track_sort: Some((2, true)),
            is_tracks_favorites_only: true,
            window_geometry: Some(WindowGeometry {
                x: -12,
                y: 40,
                width: 1280,
                height: 820,
            }),
            load_problem: None,
        };
        saved.save_to(&path).unwrap();
        assert_eq!(AppSettings::load_from(&path), saved);
        assert!(!directory.join("nested").join(TEMPORARY_FILE_NAME).exists());
    }

    #[test]
    fn save_replaces_an_existing_file() {
        let directory = fresh_test_directory("replace");
        let path = directory.join(SETTINGS_FILE_NAME);
        AppSettings::default().save_to(&path).unwrap();
        let updated = AppSettings {
            output_mode: OutputModePreference::Exclusive,
            ..AppSettings::default()
        };
        updated.save_to(&path).unwrap();
        assert_eq!(AppSettings::load_from(&path), updated);
    }

    #[test]
    fn unknown_keys_and_comments_are_ignored() {
        let settings = AppSettings::parse(
            "# comment\n\nfuture_key = 42\noutput_mode = exclusive\n  # indented comment\n",
        );
        assert_eq!(settings.output_mode, OutputModePreference::Exclusive);
        assert_eq!(settings.load_problem, None);
    }

    #[test]
    fn invalid_value_only_resets_that_key() {
        let settings = AppSettings::parse(
            "output_mode = loud\nexclusive_mode_notice_seen = true\noutput_device_id = dev1\n",
        );
        assert_eq!(settings.output_mode, OutputModePreference::Shared);
        assert!(settings.exclusive_mode_notice_seen);
        assert_eq!(
            settings.output_device,
            OutputDeviceSelection::Specific(OutputDeviceId("dev1".to_owned()))
        );
        assert!(settings.load_problem.unwrap().contains(OUTPUT_MODE_KEY));
    }

    #[test]
    fn malformed_line_is_skipped_and_reported() {
        let settings = AppSettings::parse("garbage line\noutput_mode = exclusive\n");
        assert_eq!(settings.output_mode, OutputModePreference::Exclusive);
        assert!(settings.load_problem.unwrap().contains("line 1"));
    }

    #[test]
    fn invalid_utf8_file_does_not_crash() {
        let directory = fresh_test_directory("invalid-utf8");
        let path = directory.join(SETTINGS_FILE_NAME);
        fs::write(&path, [0xff, 0xfe, b'\n', 0xc3]).unwrap();
        let settings = AppSettings::load_from(&path);
        assert_eq!(settings.output_mode, OutputModePreference::Shared);
    }

    #[test]
    fn unreadable_path_gives_defaults_with_problem() {
        let directory = fresh_test_directory("unreadable");
        // A directory where the file should be: reading it fails but is not "not found".
        let settings = AppSettings::load_from(&directory);
        assert_eq!(settings.output_mode, OutputModePreference::Shared);
        assert!(settings.load_problem.is_some());
    }

    #[test]
    fn line_break_in_a_value_is_refused_on_save() {
        let directory = fresh_test_directory("line-break");
        let settings = AppSettings {
            output_device: OutputDeviceSelection::Specific(OutputDeviceId("dev1".to_owned())),
            output_device_name: Some("bad\nname".to_owned()),
            ..AppSettings::default()
        };
        assert!(
            settings
                .save_to(&directory.join(SETTINGS_FILE_NAME))
                .is_err()
        );
        assert!(!directory.join(SETTINGS_FILE_NAME).exists());
    }

    #[test]
    fn engine_start_options_carry_mode_and_device() {
        let settings = AppSettings {
            output_device: OutputDeviceSelection::Specific(OutputDeviceId("dev1".to_owned())),
            output_mode: OutputModePreference::Exclusive,
            ..AppSettings::default()
        };
        let options = settings.engine_start_options();
        assert_eq!(
            options.output_mode_preference,
            OutputModePreference::Exclusive
        );
        assert_eq!(
            options.device_selection,
            OutputDeviceSelection::Specific(OutputDeviceId("dev1".to_owned()))
        );
    }

    #[test]
    fn the_system_output_round_trips_without_a_device_line() {
        let settings = AppSettings {
            output_device: OutputDeviceSelection::SystemDefault,
            // A name left from an earlier specific choice is not written for the system output.
            output_device_name: Some("Haut-parleurs MacBook Air".to_owned()),
            ..AppSettings::default()
        };
        let text = settings.serialize().unwrap();
        assert!(!text.contains(OUTPUT_DEVICE_ID_KEY));
        assert!(!text.contains(OUTPUT_DEVICE_NAME_KEY));
        let parsed = AppSettings::parse(&text);
        assert_eq!(parsed.output_device, OutputDeviceSelection::SystemDefault);
        assert_eq!(
            parsed.engine_start_options().device_selection,
            OutputDeviceSelection::SystemDefault
        );
    }

    #[test]
    fn a_specific_device_round_trips_with_its_name() {
        let settings = AppSettings {
            output_device: OutputDeviceSelection::Specific(OutputDeviceId(
                "AppleUSBAudioEngine:FiiO:K11".to_owned(),
            )),
            output_device_name: Some("FiiO K11".to_owned()),
            ..AppSettings::default()
        };
        assert_eq!(AppSettings::parse(&settings.serialize().unwrap()), settings);
    }

    /// Migration: a file written before "Sortie du système" existed.
    #[test]
    fn an_older_file_without_a_device_follows_the_system_output() {
        let settings =
            AppSettings::parse("output_mode = shared\nexclusive_mode_notice_seen = false\n");
        assert_eq!(settings.output_device, OutputDeviceSelection::SystemDefault);
        assert_eq!(settings.load_problem, None);
    }

    /// Migration: a device picked in an older version stays that device.
    #[test]
    fn an_older_file_with_a_device_keeps_that_device() {
        let settings = AppSettings::parse(concat!(
            "output_device_id = BuiltInSpeakerDevice\n",
            "output_device_name = Haut-parleurs MacBook Air\n",
            "output_mode = shared\n",
        ));
        assert_eq!(
            settings.output_device,
            OutputDeviceSelection::Specific(OutputDeviceId("BuiltInSpeakerDevice".to_owned()))
        );
        assert_eq!(
            settings.output_device_name.as_deref(),
            Some("Haut-parleurs MacBook Air")
        );
    }

    #[test]
    fn legacy_folder_moves_once_and_never_overwrites() {
        let root = fresh_test_directory("migration");
        let legacy = root.join("legacy");
        let new = root.join("Application Support").join("Pistolame");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(SETTINGS_FILE_NAME), "output_mode = exclusive\n").unwrap();
        fs::write(legacy.join(LIBRARY_FILE_NAME), "db").unwrap();
        assert!(move_directory_once(&legacy, &new).unwrap());
        assert!(!legacy.exists());
        assert_eq!(fs::read(new.join(LIBRARY_FILE_NAME)).unwrap(), b"db");
        // A second legacy folder is left alone once the new folder exists.
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(LIBRARY_FILE_NAME), "other").unwrap();
        assert!(!move_directory_once(&legacy, &new).unwrap());
        assert_eq!(fs::read(new.join(LIBRARY_FILE_NAME)).unwrap(), b"db");
    }

    #[test]
    fn shared_volume_round_trips_through_the_file() {
        let mut settings = AppSettings::default();
        settings.set_shared_mode_volume(SharedModeVolume::at_decibels(-12.5));
        settings.is_shared_volume_muted = true;
        let parsed = AppSettings::parse(&settings.serialize().unwrap());
        assert_eq!(parsed.shared_volume_tenths_of_decibel, -125);
        assert!(parsed.is_shared_volume_muted);
        assert_eq!(parsed.shared_mode_volume().decibels, -12.5);
    }

    #[test]
    fn start_section_round_trips() {
        let settings = AppSettings {
            start_section: Some("tracks".to_owned()),
            ..AppSettings::default()
        };
        let parsed = AppSettings::parse(&settings.serialize().unwrap());
        assert_eq!(parsed.start_section.as_deref(), Some("tracks"));
    }

    #[test]
    fn invalid_track_sort_and_window_geometry_are_ignored() {
        let settings = AppSettings::parse(
            "track_sort = 2 sideways
window_geometry = 1,2,0,4
single_click_play = maybe
",
        );
        assert_eq!(settings.track_sort, None);
        assert_eq!(settings.window_geometry, None);
        assert!(!settings.is_single_click_play);
    }

    #[test]
    fn empty_start_section_is_ignored() {
        let settings = AppSettings::parse("start_section =\n");
        assert_eq!(settings.start_section, None);
        assert_eq!(settings.load_problem, None);
    }
}
