//! Sentinel's on-device agent: a small Kotlin program (source in `agent/` at the repo root)
//! compiled to Android dex by `build.rs` and embedded here. It is pushed to the phone and run
//! with `app_process` for each mirroring session, and deletes itself once started.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, DefaultHasher, Hash, Hasher},
    path::PathBuf,
};

use anyhow::{Context, Result};

use crate::adb;

/// The compiled agent.
pub const JAR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sentinel-agent.jar"));

/// Entry point class started by `app_process`.
pub const MAIN_CLASS: &str = "dev.sentinel.agent.Main";

/// Protocol version the agent speaks; checked in the handshake.
pub const PROTOCOL_VERSION: u8 = 1;

/// A random 64-bit id, seeded from the OS (std's `RandomState`), for socket and file names.
pub(crate) fn random_id() -> u64 {
    RandomState::new().hash_one(std::process::id())
}

/// Where one agent run keeps its jar on the device. Every run gets its own file, because the
/// agent deletes its jar as soon as it starts: a shared path would let one run delete the
/// file another run is about to load (e.g. two Sentinel windows on the same phone).
pub fn device_path(id: &str) -> String {
    format!("/data/local/tmp/sentinel-agent-{id}.jar")
}

/// Remove a jar left behind by a run that never started (best effort; the agent normally
/// deletes its own).
pub fn remove(serial: &str, device_path: &str) {
    let _ = adb::run(Some(serial), &["shell", &format!("rm -f {device_path}")]);
}

/// Copy the agent to `device_path` on the device.
pub fn push_to(serial: &str, device_path: &str) -> Result<()> {
    // `adb push` needs a file on disk; name it by content so concurrent sessions share it.
    let mut hasher = DefaultHasher::new();
    JAR.hash(&mut hasher);
    let local: PathBuf = std::env::temp_dir().join(format!("sentinel-agent-{:016x}.jar", hasher.finish()));
    if std::fs::metadata(&local).map(|m| m.len() as usize).ok() != Some(JAR.len()) {
        std::fs::write(&local, JAR).with_context(|| format!("writing {}", local.display()))?;
    }
    let local = local.to_str().context("temp path is not valid UTF-8")?;
    adb::run(Some(serial), &["push", local, device_path]).context("failed to push the agent to the device")?;
    Ok(())
}
