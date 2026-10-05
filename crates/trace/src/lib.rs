//! Recorded session traces.
//!
//! One session is one meeting, kept in a directory under the data directory:
//! a `manifest.json` with the settings the meeting ran with, an append-only
//! `events.jsonl` of [`record::Record`] lines, and, when audio is switched
//! on, `audio/me.wav` and `audio/them.wav` laid out on the meeting timeline.
//! A pure crate: it owns the on-disk format, the writer, the reader and the
//! reports over traces, and depends only on `clueless-types`.

pub mod audio;
pub mod compare;
pub mod list;
pub mod manifest;
pub mod paths;
pub mod reader;
pub mod record;
pub mod show;
#[macro_use]
pub mod sink;
#[cfg(feature = "testutil")]
pub mod testutil;
pub mod writer;
