//! Capcom: private, self-hosted dictation client.

pub mod asr;
pub mod audio;
pub mod cleanup;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod history;
pub mod ipc;
pub mod pipeline;
pub mod platform;
pub mod setup;
pub mod state;
pub mod ui;
#[cfg(test)]
mod testutil;
