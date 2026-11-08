//! YuiLiveKit's core, kept apart from the Tauri window so it can be tested on its own.

pub mod core;
pub mod db;
pub mod appearance;
pub mod model;
pub mod obs_plugin;
pub mod runtime;
pub mod server;
pub mod speech;
pub mod subtitles;
pub mod translation;
pub mod tools;
pub mod twitch;
pub mod worker;
pub mod youtube;

#[cfg(test)]
mod speech_flow_tests;
#[cfg(test)]
mod media_flow_tests;
