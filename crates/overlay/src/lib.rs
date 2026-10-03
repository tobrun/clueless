//! Overlay: a standard window and a hidden always-on-top panel, plus status
//! item, main menu and hotkeys.
//!
//! The UI is intentionally dumb: every engine event maps to a [`model::UiEvent`]
//! that updates a pure [`model::UiModel`]; the AppKit views just render the
//! resulting state. Layers:
//!
//! ```text
//! mod model;        // pure state machine + clamp_origin        [unit]
//! mod mode;         // pure UI mode + frame math                [unit]
//! mod hotkeys;      // parse + register global hotkeys          [unit]
//! mod panel;        // NSPanel subclass, collection behavior    [e2e]
//! mod window;       // standard NSWindow subclass + autosave    [e2e]
//! mod views;        // status line, ticker, suggestion views    [integration]
//! mod status_item;  // menu-bar item + menu                     [e2e]
//! mod app_menu;     // hidden main menu: cmd key equivalents    [e2e]
//! pub mod ui;       // wires it all together                    [integration]
//! ```

pub mod hotkeys;
pub mod mode;
pub mod model;

#[cfg(target_os = "macos")]
mod app_menu;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(target_os = "macos")]
mod status_item;
#[cfg(target_os = "macos")]
pub mod ui;
#[cfg(target_os = "macos")]
mod views;
#[cfg(target_os = "macos")]
mod window;
