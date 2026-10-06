//! Pure input-device selection behind the Me microphone, reasoning in
//! `docs/decisions.md` (D-mic-bluetooth).
//!
//! Opening a Bluetooth headset's mic makes macOS switch the whole device
//! from A2DP to HFP, narrowing every output on it for the rest of the
//! meeting. `Auto` therefore steers the Me source onto a wired input when
//! the system default is Bluetooth. Nothing here touches CoreAudio;
//! [`crate::coreaudio`] supplies the transport and tests feed device lists.

use clueless_types::{MicChoice, StatusLevel};

/// A CoreAudio device's transport, classified into what a picker cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// the machine's own microphone
    BuiltIn,
    /// classic Bluetooth (A2DP/HFP headsets and handsets)
    Bluetooth,
    /// Bluetooth Low Energy audio (LE Audio headsets)
    BluetoothLe,
    /// a USB microphone or audio class device
    Usb,
    /// any other wired transport (PCI, Thunderbolt, DisplayPort, ...)
    Other,
    /// software devices (BlackHole, loopback, driver fakes); they carry
    /// whatever the user routes into them, usually silence
    Virtual,
    /// an aggregate device built by Audio MIDI Setup
    Aggregate,
    /// the Handoff/Continuity microphone of another device
    Continuity,
    /// an AirPlay speaker or receiver
    AirPlay,
    /// anything unrecognised
    Unknown,
}

impl Transport {
    /// Whether `Auto` may pick a device with this transport on its own.
    /// Only transports that certainly record real local sound qualify:
    /// virtual, aggregate, continuity and AirPlay inputs would silently
    /// capture nothing useful, and Unknown might be any of them.
    pub fn auto_pickable(self) -> bool {
        matches!(self, Self::BuiltIn | Self::Usb | Self::Other)
    }

    /// Whether this transport is a Bluetooth audio device whose mic drags
    /// the device into the call profile.
    pub fn is_bluetooth(self) -> bool {
        matches!(self, Self::Bluetooth | Self::BluetoothLe)
    }
}

/// Build an OSType fourCC (big-endian, as CoreAudio declares them).
pub const fn fourcc(bytes: [u8; 4]) -> u32 {
    u32::from_be_bytes(bytes)
}

// `kAudioDeviceTransportType*` fourCCs from CoreAudio's CoreAudioBaseTypes.h.
pub const K_TRANSPORT_UNKNOWN: u32 = fourcc(*b"unkn");
pub const K_TRANSPORT_BUILT_IN: u32 = fourcc(*b"bltn");
pub const K_TRANSPORT_PCI: u32 = fourcc(*b"pci ");
pub const K_TRANSPORT_USB: u32 = fourcc(*b"usb ");
pub const K_TRANSPORT_FIREWIRE: u32 = fourcc(*b"fire");
pub const K_TRANSPORT_THUNDERBOLT: u32 = fourcc(*b"thun");
pub const K_TRANSPORT_BLUETOOTH: u32 = fourcc(*b"blue");
pub const K_TRANSPORT_BLUETOOTH_LE: u32 = fourcc(*b"blea");
pub const K_TRANSPORT_HDMI: u32 = fourcc(*b"hdmi");
pub const K_TRANSPORT_DISPLAY_PORT: u32 = fourcc(*b"dprt");
pub const K_TRANSPORT_AIRPLAY: u32 = fourcc(*b"airp");
pub const K_TRANSPORT_VIRTUAL: u32 = fourcc(*b"virt");
pub const K_TRANSPORT_AGGREGATE: u32 = fourcc(*b"grup");
/// Continuity/Handoff microphones report one of three vendor fourCCs.
pub const K_TRANSPORT_CONTINUITY: u32 = fourcc(*b"ccwd");
pub const K_TRANSPORT_CONTINUITY_LE: u32 = fourcc(*b"ccwl");
pub const K_TRANSPORT_CONTINUITY_CAP: u32 = fourcc(*b"ccap");

/// Map a `kAudioDevicePropertyTransportType` value onto a [`Transport`].
/// Transports not named here (PCI, FireWire, Thunderbolt, HDMI, DisplayPort
/// and friends) are all wired hardware: they classify as `Other`.
pub fn classify(four_cc: u32) -> Transport {
    match four_cc {
        K_TRANSPORT_BUILT_IN => Transport::BuiltIn,
        K_TRANSPORT_BLUETOOTH => Transport::Bluetooth,
        K_TRANSPORT_BLUETOOTH_LE => Transport::BluetoothLe,
        K_TRANSPORT_USB => Transport::Usb,
        K_TRANSPORT_VIRTUAL => Transport::Virtual,
        K_TRANSPORT_AGGREGATE => Transport::Aggregate,
        K_TRANSPORT_CONTINUITY | K_TRANSPORT_CONTINUITY_LE | K_TRANSPORT_CONTINUITY_CAP => {
            Transport::Continuity
        }
        K_TRANSPORT_AIRPLAY => Transport::AirPlay,
        K_TRANSPORT_UNKNOWN => Transport::Unknown,
        _ => Transport::Other,
    }
}

/// One candidate input device, flattened for pure selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputInfo {
    pub name: String,
    pub transport: Transport,
    pub is_default: bool,
}

/// Why a device was picked, which is also what the status text says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// the system default, asked for or non-Bluetooth
    Default,
    /// the literal configuration name
    Exact,
    /// a wired replacement chosen over the Bluetooth default
    WiredReplacement,
    /// no wired replacement exists, the Bluetooth default it is
    BluetoothFallback,
}

/// The chosen device: index into the list and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub index: usize,
    pub reason: Reason,
}

/// Pick the input for `choice` out of `inputs`.
///
/// `Err` carries the message for a status/log: an unknown configured name
/// lists the available inputs, `SystemDefault` without a default input says
/// so. Never picks `Auto`'s replacement from the transports that would
/// record silence; if only Bluetooth remains, that is the pick with a warn.
pub fn pick(choice: &MicChoice, inputs: &[InputInfo]) -> Result<Choice, String> {
    let default = inputs.iter().position(|i| i.is_default);
    match choice {
        MicChoice::Named(want) => match inputs.iter().position(|i| &i.name == want) {
            Some(index) => Ok(Choice {
                index,
                reason: Reason::Exact,
            }),
            None => Err(format!(
                "input device \"{want}\" not found; available input devices: {}",
                list_names(inputs)
            )),
        },
        MicChoice::SystemDefault => match default {
            Some(index) => Ok(Choice {
                index,
                reason: Reason::Default,
            }),
            None => Err(format!(
                "no default input device; available input devices: {}",
                list_names(inputs)
            )),
        },
        MicChoice::Auto => {
            let Some(default) = default else {
                return Err(format!(
                    "no default input device; available input devices: {}",
                    list_names(inputs)
                ));
            };
            if !inputs[default].transport.is_bluetooth() {
                return Ok(Choice {
                    index: default,
                    reason: Reason::Default,
                });
            }
            // Allowlist order: the machine's own mic, then USB, then any
            // other wired transport.
            let want = [Transport::BuiltIn, Transport::Usb, Transport::Other];
            for transport in want {
                if let Some(index) = inputs.iter().position(|i| i.transport == transport) {
                    return Ok(Choice {
                        index,
                        reason: Reason::WiredReplacement,
                    });
                }
            }
            Ok(Choice {
                index: default,
                reason: Reason::BluetoothFallback,
            })
        }
    }
}

/// The status to show for a [`pick`] outcome: an Info naming the mic and
/// why, or a Warn when a Bluetooth mic is unavoidable.
pub fn status_for(choice: &Choice, inputs: &[InputInfo]) -> (StatusLevel, String) {
    let name = &inputs[choice.index].name;
    match choice.reason {
        Reason::Default => (StatusLevel::Info, format!("Mic: {name} (system default)")),
        Reason::Exact => (StatusLevel::Info, format!("Mic: {name} (as configured)")),
        Reason::WiredReplacement => (
            StatusLevel::Info,
            format!(
                "Mic: {name} (kept headset audio full quality, set mic_device = \"default\" to override)"
            ),
        ),
        Reason::BluetoothFallback => (
            StatusLevel::Warn,
            format!(
                "Mic: {name} (no wired mic found; headset audio drops to call quality during meetings)"
            ),
        ),
    }
}

/// Comma-separated quoted names for "available input devices" messages.
pub fn list_names(inputs: &[InputInfo]) -> String {
    if inputs.is_empty() {
        return "none".to_string();
    }
    inputs
        .iter()
        .map(|i| format!("\"{}\"", i.name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, transport: Transport, is_default: bool) -> InputInfo {
        InputInfo {
            name: name.into(),
            transport,
            is_default,
        }
    }

    /// The October 2026 developer machine: HDB 630 Bluetooth headset as
    /// default in and out, BlackHole 2ch and the built-in mic present.
    fn this_machine() -> Vec<InputInfo> {
        vec![
            input("HDB 630", Transport::Bluetooth, true),
            input("BlackHole 2ch", Transport::Virtual, false),
            input("MacBook Pro Microphone", Transport::BuiltIn, false),
        ]
    }

    #[test]
    fn auto_on_this_machines_list_picks_the_built_in_mic() {
        let inputs = this_machine();
        let choice = pick(&MicChoice::Auto, &inputs).unwrap();
        assert_eq!(
            choice,
            Choice {
                index: 2,
                reason: Reason::WiredReplacement,
            }
        );
        let (level, text) = status_for(&choice, &inputs);
        assert_eq!(level, StatusLevel::Info);
        assert!(text.starts_with("Mic: MacBook Pro Microphone"), "{text}");
    }

    #[test]
    fn auto_on_a_clamshell_list_falls_back_to_bluetooth_with_a_warn() {
        let inputs = vec![
            input("HDB 630", Transport::Bluetooth, true),
            input("BlackHole 2ch", Transport::Virtual, false),
            input("Microsoft Teams Audio", Transport::Virtual, false),
        ];
        let choice = pick(&MicChoice::Auto, &inputs).unwrap();
        assert_eq!(
            choice,
            Choice {
                index: 0,
                reason: Reason::BluetoothFallback,
            }
        );
        let (level, text) = status_for(&choice, &inputs);
        assert_eq!(level, StatusLevel::Warn);
        assert!(text.contains("call quality"), "{text}");
    }

    #[test]
    fn auto_picks_usb_when_there_is_no_built_in() {
        let inputs = vec![
            input("WH-1000XM5", Transport::Bluetooth, true),
            input("Scarlett Solo", Transport::Usb, false),
            input("Harm's Speaker", Transport::Aggregate, false),
            input("MacBook Microphone", Transport::Continuity, false),
        ];
        let choice = pick(&MicChoice::Auto, &inputs).unwrap();
        assert_eq!(inputs[choice.index].name, "Scarlett Solo");
        assert_eq!(choice.reason, Reason::WiredReplacement);
    }

    #[test]
    fn auto_leaves_a_wired_default_untouched() {
        let inputs = vec![
            input("MacBook Pro Microphone", Transport::BuiltIn, true),
            input("HDB 630", Transport::Bluetooth, false),
        ];
        let choice = pick(&MicChoice::Auto, &inputs).unwrap();
        assert_eq!(
            choice,
            Choice {
                index: 0,
                reason: Reason::Default,
            }
        );
        assert_eq!(
            status_for(&choice, &inputs).1,
            "Mic: MacBook Pro Microphone (system default)"
        );
    }

    #[test]
    fn system_default_keeps_the_bluetooth_default() {
        let inputs = this_machine();
        let choice = pick(&MicChoice::SystemDefault, &inputs).unwrap();
        assert_eq!(inputs[choice.index].name, "HDB 630");
        assert_eq!(choice.reason, Reason::Default);
    }

    #[test]
    fn named_keeps_bluetooth_as_the_explicit_opt_in() {
        let inputs = this_machine();
        let choice = pick(&MicChoice::Named("HDB 630".into()), &inputs).unwrap();
        assert_eq!(inputs[choice.index].name, "HDB 630");
        assert_eq!(choice.reason, Reason::Exact);
        assert_eq!(
            status_for(&choice, &inputs).1,
            "Mic: HDB 630 (as configured)"
        );
    }

    #[test]
    fn a_missing_name_lists_the_available_inputs() {
        let inputs = this_machine();
        let err = pick(&MicChoice::Named("NoSuchMic".into()), &inputs).unwrap_err();
        assert!(err.contains("\"NoSuchMic\" not found"), "{err}");
        assert!(err.contains("\"HDB 630\""), "{err}");
        assert!(err.contains("\"BlackHole 2ch\""), "{err}");
        assert!(err.contains("\"MacBook Pro Microphone\""), "{err}");
    }

    #[test]
    fn classify_maps_the_coreaudio_constants() {
        assert_eq!(classify(K_TRANSPORT_BUILT_IN), Transport::BuiltIn);
        assert_eq!(classify(K_TRANSPORT_BLUETOOTH), Transport::Bluetooth);
        assert_eq!(classify(K_TRANSPORT_BLUETOOTH_LE), Transport::BluetoothLe);
        assert_eq!(classify(K_TRANSPORT_USB), Transport::Usb);
        assert_eq!(classify(K_TRANSPORT_VIRTUAL), Transport::Virtual);
        assert_eq!(classify(fourcc(*b"loop")), Transport::Other);
        assert_eq!(classify(K_TRANSPORT_AGGREGATE), Transport::Aggregate);
        assert_eq!(classify(K_TRANSPORT_CONTINUITY), Transport::Continuity);
        assert_eq!(classify(K_TRANSPORT_CONTINUITY_LE), Transport::Continuity);
        assert_eq!(classify(K_TRANSPORT_CONTINUITY_CAP), Transport::Continuity);
        assert_eq!(classify(K_TRANSPORT_AIRPLAY), Transport::AirPlay);
        assert_eq!(classify(K_TRANSPORT_UNKNOWN), Transport::Unknown);
        assert_eq!(classify(K_TRANSPORT_THUNDERBOLT), Transport::Other);
        // anything unrecognised is Other
        assert_eq!(classify(0xDEAD_BEEF), Transport::Other);
    }

    #[test]
    fn auto_never_pick_the_silent_transports() {
        for t in [
            Transport::Virtual,
            Transport::Aggregate,
            Transport::Continuity,
            Transport::AirPlay,
            Transport::Unknown,
        ] {
            assert!(!t.auto_pickable(), "{t:?} must not be auto-picked");
        }
        for t in [Transport::BuiltIn, Transport::Usb, Transport::Other] {
            assert!(t.auto_pickable(), "{t:?} must be auto-pickable");
        }
    }
}
