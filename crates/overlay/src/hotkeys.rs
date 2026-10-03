//! Global hotkeys: config parsing and the command routing the overlay acts
//! on. One [`global_hotkey::GlobalHotKeyManager`] is created on the main
//! thread in [`crate::ui::run`] and kept alive there. Always-on keys are
//! registered at startup; meeting-only keys (suggest, clear, the four move
//! keys) only while a meeting runs (the rationale is in `docs/decisions.md`).

use clueless_types::EngineCommand;
use clueless_types::config::HotkeysConfig;
use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::HotKey;

/// What an overlay hotkey does. Engine commands go to the engine; the rest
/// are local overlay actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    Suggest,
    ClearSuggestion,
    ToggleMeeting,
    /// Show or hide the panel.
    ToggleOverlay,
    /// Switch between the standard window and the hidden overlay.
    ToggleMode,
    /// Turn interactive mode on or off (panel becomes key-able).
    ToggleClickThrough,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
}

/// Points one press of a move key shifts the panel (spec: 40 points).
pub const MOVE_STEP: f64 = 40.0;

impl HotkeyAction {
    /// Meeting-only keys are registered on `MeetingState(Running)` and
    /// unregistered on `Idle`; everything else is always on. The move keys
    /// are meeting-only so `cmd+shift+arrows` stays available to other apps
    /// outside a meeting (spec D-move-keys).
    pub fn meeting_only(&self) -> bool {
        matches!(
            self,
            Self::Suggest
                | Self::ClearSuggestion
                | Self::MoveLeft
                | Self::MoveRight
                | Self::MoveUp
                | Self::MoveDown
        )
    }

    /// The engine command this action sends, when it is not local.
    pub fn engine_command(&self) -> Option<EngineCommand> {
        match self {
            Self::Suggest => Some(EngineCommand::Suggest),
            Self::ClearSuggestion => Some(EngineCommand::ClearSuggestion),
            Self::ToggleMeeting => Some(EngineCommand::ToggleMeeting),
            _ => None,
        }
    }
}

/// One parsed hotkey with its provenance, so a registration failure can name
/// the key and the config field it came from.
#[derive(Debug, Clone)]
pub struct ParsedHotkey {
    pub action: HotkeyAction,
    /// Config field name, e.g. `hotkeys.toggle_overlay`.
    pub field: &'static str,
    pub raw: String,
    pub hotkey: HotKey,
}

/// A hotkey string that does not parse: names the key string and the config
/// field so the user can fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyParseError {
    pub field: &'static str,
    pub raw: String,
    pub reason: String,
}

impl std::fmt::Display for HotkeyParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hotkeys.{}: could not parse hotkey {:?} ({})",
            self.field, self.raw, self.reason
        )
    }
}

impl std::error::Error for HotkeyParseError {}

fn table(config: &HotkeysConfig) -> [(HotkeyAction, &'static str, &str); 10] {
    [
        (HotkeyAction::Suggest, "suggest", &config.suggest),
        (HotkeyAction::ClearSuggestion, "clear", &config.clear),
        (HotkeyAction::MoveLeft, "move_left", &config.move_left),
        (HotkeyAction::MoveRight, "move_right", &config.move_right),
        (HotkeyAction::MoveUp, "move_up", &config.move_up),
        (HotkeyAction::MoveDown, "move_down", &config.move_down),
        (
            HotkeyAction::ToggleMeeting,
            "toggle_meeting",
            &config.toggle_meeting,
        ),
        (
            HotkeyAction::ToggleOverlay,
            "toggle_overlay",
            &config.toggle_overlay,
        ),
        (HotkeyAction::ToggleMode, "toggle_mode", &config.toggle_mode),
        (
            HotkeyAction::ToggleClickThrough,
            "toggle_click_through",
            &config.toggle_click_through,
        ),
    ]
}

/// Parse every configured hotkey. The first bad string is an error naming
/// the key and the field; callers show it and keep the rest.
pub fn parse_all(config: &HotkeysConfig) -> Result<Vec<ParsedHotkey>, HotkeyParseError> {
    let mut out = Vec::with_capacity(10);
    for (action, field, raw) in table(config) {
        match raw.parse::<HotKey>() {
            Ok(hotkey) => out.push(ParsedHotkey {
                action,
                field,
                raw: raw.to_string(),
                hotkey,
            }),
            Err(err) => {
                return Err(HotkeyParseError {
                    field,
                    raw: raw.to_string(),
                    reason: err.to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// Register or unregister the meeting-only keys to match `meeting`. Returns
/// the actions whose registration changed so the caller can report
/// failures. A failed meeting-key registration is not fatal: the always-on
/// keys keep working (spec: bad hotkey shows a status error, app keeps
/// running).
pub fn sync_meeting_keys(
    manager: &GlobalHotKeyManager,
    all: &[ParsedHotkey],
    meeting: bool,
    registered: &mut Vec<(u32, HotkeyAction)>,
) -> Vec<Result<HotkeyAction, String>> {
    let mut results = Vec::new();
    if meeting {
        for parsed in all.iter().filter(|p| p.action.meeting_only()) {
            if registered.iter().any(|(id, _)| *id == parsed.hotkey.id) {
                continue;
            }
            match manager.register(parsed.hotkey) {
                Ok(()) => registered.push((parsed.hotkey.id, parsed.action)),
                Err(err) => results.push(Err(format!(
                    "hotkey {} ({}) could not be registered: {err}",
                    parsed.raw, parsed.field
                ))),
            }
        }
    } else {
        registered.retain(|(id, action)| {
            if !action.meeting_only() {
                return true;
            }
            for parsed in all.iter().filter(|p| p.action.meeting_only()) {
                if parsed.hotkey.id == *id {
                    if let Err(err) = manager.unregister(parsed.hotkey) {
                        tracing::debug!(%err, id, "meeting hotkey unregister failed");
                    }
                    return false;
                }
            }
            true
        });
    }
    results
}

/// Build the id -> action map for always-on keys after registering them.
/// Failures are returned as messages naming the key; the app runs without
/// that key.
pub fn register_always_on(
    manager: &GlobalHotKeyManager,
    all: &[ParsedHotkey],
) -> (Vec<(u32, HotkeyAction)>, Vec<String>) {
    let mut registered = Vec::new();
    let mut errors = Vec::new();
    for parsed in all.iter().filter(|p| !p.action.meeting_only()) {
        match manager.register(parsed.hotkey) {
            Ok(()) => registered.push((parsed.hotkey.id, parsed.action)),
            Err(err) => errors.push(format!(
                "hotkey {} ({}) could not be registered: {err}",
                parsed.raw, parsed.field
            )),
        }
    }
    (registered, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ten_default_hotkeys_parse() {
        let config = HotkeysConfig::default();
        let parsed = parse_all(&config).expect("defaults must parse");
        assert_eq!(parsed.len(), 10);
        // Every action appears exactly once and ids are unique (a duplicate
        // id would misroute presses).
        let mut actions: Vec<HotkeyAction> = parsed.iter().map(|p| p.action).collect();
        actions.sort_by_key(|a| format!("{a:?}"));
        let count = actions.len();
        actions.dedup();
        assert_eq!(actions.len(), count);
        let mut ids: Vec<u32> = parsed.iter().map(|p| p.hotkey.id).collect();
        ids.sort_unstable();
        let id_count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), id_count);
    }

    #[test]
    fn an_unparseable_hotkey_names_the_key_and_the_field() {
        let config = HotkeysConfig {
            move_left: "cmd+Nope".into(),
            ..HotkeysConfig::default()
        };
        let err = parse_all(&config).expect_err("cmd+Nope must not parse");
        let message = err.to_string();
        assert!(message.contains("Nope"), "names the key: {message}");
        assert!(message.contains("move_left"), "names the field: {message}");
    }

    #[test]
    fn meeting_only_set_is_suggest_clear_and_the_four_moves() {
        for action in [
            HotkeyAction::Suggest,
            HotkeyAction::ClearSuggestion,
            HotkeyAction::MoveLeft,
            HotkeyAction::MoveRight,
            HotkeyAction::MoveUp,
            HotkeyAction::MoveDown,
        ] {
            assert!(action.meeting_only(), "{action:?} must be meeting-only");
        }
        for action in [
            HotkeyAction::ToggleMeeting,
            HotkeyAction::ToggleOverlay,
            HotkeyAction::ToggleMode,
            HotkeyAction::ToggleClickThrough,
        ] {
            assert!(!action.meeting_only(), "{action:?} must be always-on");
        }
    }
}
