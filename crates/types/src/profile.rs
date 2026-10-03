//! The assist profiles: the three built-in kinds of help the user can switch
//! between, and who started a request.

use std::str::FromStr;

use serde::{Deserialize, Deserializer};

/// Which kind of help the app gives. `Manual` is today's hotkey-only
/// behaviour; the other two start requests by themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AssistProfile {
    #[default]
    Manual,
    Interview,
    Brainstorm,
}

impl AssistProfile {
    /// Every profile, in the order `next` cycles through them.
    pub const ALL: [AssistProfile; 3] = [Self::Manual, Self::Interview, Self::Brainstorm];

    /// The name shown to the user.
    pub fn name(self) -> &'static str {
        match self {
            Self::Manual => "Manual",
            Self::Interview => "Interview",
            Self::Brainstorm => "Brainstorm",
        }
    }

    /// The lowercase key used in config files and on the command line.
    pub fn key(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Interview => "interview",
            Self::Brainstorm => "brainstorm",
        }
    }

    /// The profile after this one; the last one wraps to the first.
    pub fn next(self) -> Self {
        match self {
            Self::Manual => Self::Interview,
            Self::Interview => Self::Brainstorm,
            Self::Brainstorm => Self::Manual,
        }
    }
}

/// The text that names the valid values in an error message.
pub const PROFILE_KEYS_HINT: &str = "manual, interview or brainstorm";

impl FromStr for AssistProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.key() == value)
            .ok_or_else(|| format!("unknown profile \"{value}\": expected {PROFILE_KEYS_HINT}"))
    }
}

impl<'de> Deserialize<'de> for AssistProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Who started a suggestion request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The user pressed the suggest hotkey.
    Manual,
    /// The app started it because the active profile's trigger fired.
    Auto,
}
