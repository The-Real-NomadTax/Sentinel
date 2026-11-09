//! Thin, blocking wrappers around the `adb` command-line tool.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};

/// The `adb` binary to use. Override with the `SENTINEL_ADB` environment variable.
pub fn adb_binary() -> String {
    std::env::var("SENTINEL_ADB").unwrap_or_else(|_| "adb".into())
}

/// A `Command` for `adb`, optionally targeting one device by serial.
pub fn command(serial: Option<&str>) -> Command {
    let mut cmd = Command::new(adb_binary());
    if let Some(serial) = serial {
        cmd.args(["-s", serial]);
    }
    cmd
}

/// Run an adb command and fail with its stderr/stdout if it exits non-zero.
pub(crate) fn run(serial: Option<&str>, args: &[&str]) -> Result<String> {
    let output = command(serial)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{} {}`", adb_binary(), args.join(" ")))?;
    check(&output, args)?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn check(output: &Output, args: &[&str]) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let msg = if stderr.trim().is_empty() { stdout } else { stderr };
    bail!("`adb {}` failed: {}", args.join(" "), msg.trim())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceState {
    /// Connected and authorized for debugging.
    Device,
    /// Connected, but the user hasn't accepted the RSA key prompt on the phone.
    Unauthorized,
    Offline,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: DeviceState,
    /// Human-readable model, e.g. "SM S721U" (from `adb devices -l`).
    pub model: Option<String>,
}

impl Device {
    pub fn is_ready(&self) -> bool {
        self.state == DeviceState::Device
    }

    pub fn display_name(&self) -> String {
        match &self.model {
            Some(model) => format!("{model} ({})", self.serial),
            None => self.serial.clone(),
        }
    }
}

/// List devices known to the adb server (`adb devices -l`).
pub fn list_devices() -> Result<Vec<Device>> {
    let out = run(None, &["devices", "-l"])?;
    Ok(parse_devices(&out))
}

fn parse_devices(out: &str) -> Vec<Device> {
    out.lines()
        .skip_while(|line| !line.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = match parts.next()? {
                "device" => DeviceState::Device,
                "unauthorized" => DeviceState::Unauthorized,
                "offline" => DeviceState::Offline,
                other => DeviceState::Other(other.to_string()),
            };
            let model = parts
                .find_map(|kv| kv.strip_prefix("model:"))
                .map(|m| m.replace('_', " "));
            Some(Device { serial, state, model })
        })
        .collect()
}

/// Install (or reinstall, keeping data) an APK on the device. Blocks until done.
/// Returns adb's output on success.
pub fn install_apk(serial: &str, apk: &Path) -> Result<String> {
    let apk_str = apk.to_str().context("APK path is not valid UTF-8")?;
    let out = run(Some(serial), &["install", "-r", apk_str])?;
    // Older adb versions exit 0 even when the install fails, so also check the text.
    if !out.contains("Success") {
        bail!("install failed: {}", out.trim());
    }
    Ok(out.trim().to_string())
}

/// Install an app split into several APKs (a base APK plus splits) in one go, e.g. a folder
/// exported by [`crate::apps::export_app`]. Blocks until done.
pub fn install_apks(serial: &str, apks: &[PathBuf]) -> Result<String> {
    if apks.is_empty() {
        bail!("no APK files to install");
    }
    let mut args = vec!["install-multiple".to_string(), "-r".to_string()];
    for apk in apks {
        args.push(apk.to_str().context("APK path is not valid UTF-8")?.to_string());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(Some(serial), &args)?;
    if !out.contains("Success") {
        bail!("install failed: {}", out.trim());
    }
    Ok(out.trim().to_string())
}

/// The `.apk` files directly inside `dir`, sorted (`base.apk` sorts before `split_*.apk`).
pub fn apks_in_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut apks: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("apk")))
        .collect();
    apks.sort();
    Ok(apks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices() {
        let out = "List of devices attached\n\
                   R5CXC1L90EA            device usb:3-1 product:r12ssqw model:SM_S721U device:r12s transport_id:7\n\
                   emulator-5554          unauthorized transport_id:2\n\n";
        let devices = parse_devices(out);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "R5CXC1L90EA");
        assert!(devices[0].is_ready());
        assert_eq!(devices[0].model.as_deref(), Some("SM S721U"));
        assert_eq!(devices[1].state, DeviceState::Unauthorized);
    }
}
