//! The microphone a session records from. Claude Code's voice dictation reads the
//! PulseAudio source named in `PULSE_SOURCE`, and the system default on this
//! machine is a loopback device that hears nothing.
//!
//! The choice is fixed in the code for the author's machine. It becomes a setting
//! when a second machine needs it.

use std::fmt;
use std::process::Command;

use anyhow::{Result, bail};
use serde::Deserialize;

/// The variable the PulseAudio client library reads for its input device.
pub const PULSE_SOURCE: &str = "PULSE_SOURCE";

/// Wanted sources, best first. Each matches a source whose name or description
/// holds the text. NoiseTorch names its filter after the device it cleans, so the
/// second entry is the device NoiseTorch picks when it runs.
const WANTED: [&str; 2] = ["NoiseTorch", "Family 17h/19h"];

/// One input the sound server offers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AudioSource {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The sink a monitor source listens to. Empty or absent for a real input.
    #[serde(default)]
    pub monitor_source: Option<String>,
}

impl AudioSource {
    /// True for a source that records what a sink plays rather than a microphone.
    #[must_use]
    pub fn is_monitor(&self) -> bool {
        self.monitor_source.as_ref().is_some_and(|m| !m.is_empty())
    }
}

/// Lists the sound server's sources. A process call, so it enters through a trait.
pub trait Microphones: fmt::Debug {
    fn sources(&self) -> Result<Vec<AudioSource>>;
}

/// Asks `pactl`, which talks to PulseAudio and to PipeWire's PulseAudio server.
#[derive(Debug, Default, Clone, Copy)]
pub struct PactlMicrophones;

impl Microphones for PactlMicrophones {
    fn sources(&self) -> Result<Vec<AudioSource>> {
        let out = Command::new("pactl")
            .args(["-f", "json", "list", "sources"])
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("pactl: {}", stderr.trim().replace('\n', " "));
        }
        parse_sources(&out.stdout)
    }
}

/// Reads the output of `pactl -f json list sources`.
pub fn parse_sources(json: &[u8]) -> Result<Vec<AudioSource>> {
    Ok(serde_json::from_slice(json)?)
}

/// The source a session records from, or `None` when no wanted source exists.
/// A monitor never wins, although its description names the same device.
#[must_use]
pub fn pick(sources: &[AudioSource]) -> Option<&str> {
    WANTED.iter().find_map(|wanted| {
        sources
            .iter()
            .filter(|s| !s.is_monitor())
            .find(|s| {
                s.name.contains(wanted)
                    || s.description.as_ref().is_some_and(|d| d.contains(wanted))
            })
            .map(|s| s.name.as_str())
    })
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    fn source(name: &str, description: &str, monitor_of: &str) -> AudioSource {
        AudioSource {
            name: name.into(),
            description: Some(description.into()),
            monitor_source: Some(monitor_of.into()),
        }
    }

    const NOISETORCH: &str = "NoiseTorch Microphone for Family 17h/19h HD Audio Controller";
    const DEVICE: &str = "alsa_input.pci-0000_08_00.6.analog-stereo";

    fn machine() -> Vec<AudioSource> {
        vec![
            source(
                "alsa_output.pci-0000_08_00.6.analog-stereo.monitor",
                "Monitor of Family 17h/19h HD Audio Controller Analog Stereo",
                "alsa_output.pci-0000_08_00.6.analog-stereo",
            ),
            source(
                "alsa_input.platform-snd_aloop.0.analog-stereo",
                "Loopback Analog Stereo",
                "",
            ),
            source(
                DEVICE,
                "Family 17h/19h HD Audio Controller Analog Stereo",
                "",
            ),
            source(NOISETORCH, &format!("{NOISETORCH} Source"), ""),
        ]
    }

    #[test]
    fn noisetorch_wins_while_it_runs() {
        assert_eq!(pick(&machine()), Some(NOISETORCH));
    }

    #[test]
    fn the_device_wins_by_its_description_when_noisetorch_is_off() {
        let sources: Vec<_> = machine()
            .into_iter()
            .filter(|s| s.name != NOISETORCH)
            .collect();
        assert_eq!(pick(&sources), Some(DEVICE));
    }

    #[test]
    fn a_monitor_of_the_device_never_wins() {
        let sources: Vec<_> = machine()
            .into_iter()
            .filter(|s| s.name != NOISETORCH && s.name != DEVICE)
            .collect();
        assert_eq!(pick(&sources), None);
    }

    #[test]
    fn pactl_json_reads_into_sources() {
        let json = br#"[{"index":53,"name":"a.monitor","description":"Monitor of A","monitor_source":"a","flags":[]},
                        {"index":57,"name":"b","description":"B","monitor_source":"","properties":{}},
                        {"index":58,"name":"c","monitor_source":null}]"#;
        let sources = parse_sources(json).expect("valid pactl output");
        assert_eq!(sources.len(), 3);
        assert!(!sources[2].is_monitor());
        assert!(sources[0].is_monitor());
        assert!(!sources[1].is_monitor());
        assert_eq!(sources[1].description.as_deref(), Some("B"));
    }
}
