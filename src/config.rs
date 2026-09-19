//! Settings that apply to every project, read from one TOML file.
//!
//! The file is optional. A missing file, a missing key and an empty file all read as
//! the defaults, so culm runs with no configuration at all and a user only writes
//! down what they want changed.

use serde::{Deserialize, Serialize};

/// Everything culm reads from `config.toml`.
///
/// Key bindings belong here too one day. Only the settings below are read so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Ask a paused session what it was doing, and keep the answer.
    ///
    /// Off by default, because every pause then costs one call to the model.
    pub recap_on_pause: bool,
}

/// The name a setting carries on the command line and in the file.
pub const RECAP_ON_PAUSE: &str = "recap-on-pause";

impl Config {
    /// Every setting and its current value, in the order the listing shows them.
    #[must_use]
    pub fn settings(&self) -> Vec<(&'static str, String)> {
        vec![(RECAP_ON_PAUSE, self.recap_on_pause.to_string())]
    }

    /// Applies one setting by the name the command line uses.
    ///
    /// Reports an unknown key and an unreadable value separately, because the two
    /// are different mistakes and the user fixes them differently.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), SetError> {
        match key {
            RECAP_ON_PAUSE => {
                self.recap_on_pause = parse_bool(value).ok_or(SetError::Value)?;
                Ok(())
            }
            _ => Err(SetError::Key),
        }
    }
}

/// Why a setting was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetError {
    /// No setting carries that name.
    Key,
    /// The value does not read as the type the setting holds.
    Value,
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_recap_is_off_until_it_is_asked_for() {
        assert!(!Config::default().recap_on_pause);
    }

    #[test]
    fn an_empty_file_reads_as_the_defaults() {
        let config: Config = toml::from_str("").expect("an empty file is valid");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn a_file_missing_a_key_reads_that_key_as_its_default() {
        let config: Config = toml::from_str("# nothing set here\n").expect("a comment is valid");
        assert!(!config.recap_on_pause);
    }

    #[test]
    fn a_setting_survives_a_round_trip_through_toml() {
        let config = Config {
            recap_on_pause: true,
        };
        let text = toml::to_string(&config).expect("the config serializes");
        let back: Config = toml::from_str(&text).expect("and reads back");
        assert!(back.recap_on_pause);
    }

    #[test]
    fn a_setting_is_written_by_the_name_the_command_line_uses() {
        let mut config = Config::default();
        config.set(RECAP_ON_PAUSE, "true").expect("a known setting");
        assert!(config.recap_on_pause);
        config.set(RECAP_ON_PAUSE, "off").expect("a known setting");
        assert!(!config.recap_on_pause);
    }

    #[test]
    fn an_unknown_setting_is_refused_by_name() {
        assert_eq!(
            Config::default().set("recap-on-quit", "true"),
            Err(SetError::Key)
        );
    }

    #[test]
    fn an_unreadable_value_is_refused_on_its_own() {
        assert_eq!(
            Config::default().set(RECAP_ON_PAUSE, "perhaps"),
            Err(SetError::Value)
        );
    }
}
