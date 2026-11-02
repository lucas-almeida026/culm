//! The microphone a session records from. Claude Code's voice dictation reads the
//! PulseAudio source named in `PULSE_SOURCE`. The system default is not always a
//! microphone: on the maintainer's machine it is a loopback device that hears
//! nothing.
//!
//! The `microphones` setting names the wanted sources, best first.

use std::fmt;
use std::process::Command;

use anyhow::{Result, bail};
use serde::Deserialize;

/// The variable the PulseAudio client library reads for its input device.
pub const PULSE_SOURCE: &str = "PULSE_SOURCE";

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

/// The inputs a session can record from: every source but the monitors, which
/// record what a sink plays.
#[must_use]
pub fn inputs(sources: &[AudioSource]) -> Vec<&AudioSource> {
    sources.iter().filter(|s| !s.is_monitor()).collect()
}

/// The source a session records from, or `None` when no wanted source exists.
///
/// `wanted` holds texts, best first. A source matches a text when its name or its
/// description holds it, whatever the case. A monitor never wins, although its
/// description names the same device.
#[must_use]
pub fn pick<'a>(sources: &'a [AudioSource], wanted: &[String]) -> Option<&'a str> {
    wanted.iter().find_map(|text| {
        let text = text.to_lowercase();
        inputs(sources)
            .into_iter()
            .find(|s| {
                s.name.to_lowercase().contains(&text)
                    || s.description
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(&text))
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

    fn wanted() -> Vec<String> {
        vec!["NoiseTorch".into(), "Family 17h/19h".into()]
    }

    #[test]
    fn noisetorch_wins_while_it_runs() {
        assert_eq!(pick(&machine(), &wanted()), Some(NOISETORCH));
    }

    #[test]
    fn the_device_wins_by_its_description_when_noisetorch_is_off() {
        let sources: Vec<_> = machine()
            .into_iter()
            .filter(|s| s.name != NOISETORCH)
            .collect();
        assert_eq!(pick(&sources, &wanted()), Some(DEVICE));
    }

    #[test]
    fn a_monitor_of_the_device_never_wins() {
        let sources: Vec<_> = machine()
            .into_iter()
            .filter(|s| s.name != NOISETORCH && s.name != DEVICE)
            .collect();
        assert_eq!(pick(&sources, &wanted()), None);
    }

    #[test]
    fn the_list_order_wins_over_the_order_of_the_sources() {
        let wanted = vec!["family".to_string(), "noisetorch".to_string()];
        assert_eq!(
            pick(&machine(), &wanted),
            Some(DEVICE),
            "the first text that matches decides, whatever the case"
        );
    }

    #[test]
    fn an_empty_list_picks_nothing() {
        assert_eq!(pick(&machine(), &[]), None);
    }

    #[test]
    fn the_inputs_leave_the_monitors_out() {
        let machine = machine();
        let names: Vec<&str> = inputs(&machine).iter().map(|s| s.name.as_str()).collect();
        assert!(!names.iter().any(|n| n.ends_with(".monitor")));
        assert_eq!(names.len(), 3);
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
