//! The `clueless` binary's parts, exposed as a library so integration
//! tests can drive the CLI parser, the single-instance lock and logging
//! directly; `main.rs` wires them into the two run modes.

pub mod cli;
pub mod envfile;
pub mod lock;
pub mod logging;
