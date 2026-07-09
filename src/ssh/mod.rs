//! SSH transport: russh server plus the ratatui-over-channel terminal bridge.

pub mod server;
pub mod terminal;

pub use server::run;
