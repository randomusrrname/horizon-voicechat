//! The mod's saved settings: which microphone to use and how much to amplify it.
//!
//! They live in the mod's data folder (`host.data_dir("voicechat_client")`), a tiny `settings.txt` so
//! nothing else has to be added to Horizon's `Cargo.lock`. The format is one `key=value` per line
//! and unknown keys are ignored, so an older file keeps working after an update.

use std::fs;
use std::path::{Path, PathBuf};

/// The file inside the mod's data folder.
pub const FILE: &str = "settings.txt";
/// Gain is a multiplier on the captured samples.
pub const MIN_GAIN: f32 = 0.25;
pub const MAX_GAIN: f32 = 4.0;
pub const GAIN_STEP: f32 = 0.25;
pub const DEFAULT_GAIN: f32 = 1.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The input device by name; `None` follows the system's default microphone.
    pub input: Option<String>,
    /// Microphone gain, `MIN_GAIN` to `MAX_GAIN`.
    pub gain: f32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { input: None, gain: DEFAULT_GAIN }
    }
}

impl Settings {
    /// Reads a settings file. A missing or unreadable file is the defaults, never an error.
    pub fn load(dir: &Path) -> Settings {
        match fs::read_to_string(Self::path(dir)) {
            Ok(text) => Settings::parse(&text),
            Err(_) => Settings::default(),
        }
    }

    /// Writes the file, creating the mod's folder the first time.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        fs::write(Self::path(dir), self.serialize())
            .map_err(|e| format!("could not write {}: {e}", Self::path(dir).display()))
    }

    pub fn path(dir: &Path) -> PathBuf {
        dir.join(FILE)
    }

    /// The current gain as the percentage the menu shows.
    pub fn gain_percent(&self) -> u32 {
        (self.gain * 100.0).round() as u32
    }

    /// Sets a gain, clamped to what the microphone callback can use.
    pub fn set_gain(&mut self, gain: f32) {
        if gain.is_finite() {
            self.gain = gain.clamp(MIN_GAIN, MAX_GAIN);
        }
    }

    /// Moves the gain by `delta`, e.g. one `GAIN_STEP` from the menu's buttons.
    pub fn nudge_gain(&mut self, delta: f32) {
        self.set_gain(self.gain + delta);
    }

    pub fn serialize(&self) -> String {
        let mut text = String::new();
        if let Some(input) = &self.input {
            // A newline would break the line format; device names never need one.
            text.push_str("device=");
            text.push_str(&input.replace('\n', " ").replace('\r', " "));
            text.push('\n');
        }
        text.push_str(&format!("gain={:.2}\n", self.gain));
        text
    }

    pub fn parse(text: &str) -> Settings {
        let mut settings = Settings::default();
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("device=") {
                if !value.trim().is_empty() {
                    settings.input = Some(value.to_owned());
                }
            } else if let Some(value) = line.strip_prefix("gain=") {
                if let Ok(gain) = value.trim().parse::<f32>() {
                    settings.set_gain(gain);
                }
            }
        }
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_the_file_format() {
        let original = Settings {
            input: Some("Microphone (USB Audio Device)".to_owned()),
            gain: 1.75,
        };
        assert_eq!(Settings::parse(&original.serialize()), original);
    }

    #[test]
    fn a_missing_or_broken_setting_falls_back_to_the_default() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(Settings::parse("gain=fast\nnonsense\n"), Settings::default());
        assert_eq!(Settings::parse("device=\n"), Settings::default());
    }

    #[test]
    fn gain_is_clamped_to_the_range_the_button_uses() {
        let mut settings = Settings::default();
        settings.set_gain(99.0);
        assert_eq!(settings.gain, MAX_GAIN);
        settings.set_gain(-4.0);
        assert_eq!(settings.gain, MIN_GAIN);
        settings.set_gain(f32::NAN);
        assert_eq!(settings.gain, MIN_GAIN);
        settings.nudge_gain(GAIN_STEP);
        assert_eq!(settings.gain, MIN_GAIN + GAIN_STEP);
    }

    #[test]
    fn the_file_stays_inside_the_given_folder() {
        let dir = std::env::temp_dir().join("voicechat-settings-test");
        let settings = Settings { input: None, gain: 2.0 };
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), settings);
        let _ = fs::remove_dir_all(&dir);
    }
}
