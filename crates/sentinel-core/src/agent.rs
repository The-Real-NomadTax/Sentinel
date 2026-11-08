//! Sentinel's on-device agent: a small Kotlin program (source in `agent/` at the repo root)
//! compiled to Android dex by `build.rs` and embedded here. It is pushed to the phone and run
//! with `app_process` for each mirroring session, and deletes itself once started.

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
};

use anyhow::{Context, Result};

use crate::adb;

/// The compiled agent.
pub const JAR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sentinel-agent.jar"));

/// Where the agent is placed on the device.
pub const DEVICE_PATH: &str = "/data/local/tmp/sentinel-agent.jar";

/// Entry point class started by `app_process`.
pub const MAIN_CLASS: &str = "dev.sentinel.agent.Main";

/// Protocol version the agent speaks; checked in the handshake.
pub const PROTOCOL_VERSION: u8 = 1;

/// Copy the agent to the device.
pub fn push(serial: &str) -> Result<()> {
    // `adb push` needs a file on disk; name it by content so concurrent sessions share it.
    let mut hasher = DefaultHasher::new();
    JAR.hash(&mut hasher);
    let local: PathBuf = std::env::temp_dir().join(format!("sentinel-agent-{:016x}.jar", hasher.finish()));
    if std::fs::metadata(&local).map(|m| m.len() as usize).ok() != Some(JAR.len()) {
        std::fs::write(&local, JAR).with_context(|| format!("writing {}", local.display()))?;
    }
    let local = local.to_str().context("temp path is not valid UTF-8")?;
    adb::run(Some(serial), &["push", local, DEVICE_PATH]).context("failed to push the agent to the device")?;
    Ok(())
}
