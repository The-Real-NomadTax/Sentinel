//! Device details for the dashboard: model, Android version, battery, storage, memory.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::adb;

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub serial: String,
    pub manufacturer: String,
    pub model: String,
    /// Internal device codename, e.g. "r12s".
    pub codename: String,
    /// User-facing version, e.g. "17".
    pub android_version: String,
    pub sdk: Option<u32>,
    /// e.g. "2026-10-05".
    pub security_patch: Option<String>,
    pub chipset: Chipset,
    pub battery: Option<Battery>,
    pub storage: Option<Storage>,
    pub memory: Option<Memory>,
    pub display: Option<Display>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chipset {
    /// e.g. "Samsung", "QTI", "Mediatek" (from `ro.soc.manufacturer`, Android 12+).
    pub manufacturer: Option<String>,
    /// Raw SoC code, e.g. "s5e9945" or "SM8650".
    pub model: Option<String>,
    /// Marketing name when we recognize the code, e.g. "Exynos 2400".
    pub name: Option<String>,
    /// Board platform, e.g. "erd9945", "pineapple".
    pub platform: Option<String>,
    pub cpu_abi: Option<String>,
    pub cpu_cores: Option<u32>,
    /// CPU clusters as (core count, max frequency in MHz), fastest first.
    pub cpu_clusters: Vec<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    /// 0-100.
    pub level: u8,
    pub status: BatteryStatus,
    pub health: BatteryHealth,
    pub temperature_c: Option<f32>,
    pub voltage_mv: Option<u32>,
    pub power_source: Option<PowerSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryStatus {
    Charging,
    Discharging,
    NotCharging,
    Full,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryHealth {
    Good,
    Overheat,
    Dead,
    OverVoltage,
    Cold,
    Failure,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSource {
    Ac,
    Usb,
    Wireless,
    Dock,
}

/// Internal shared storage (`/data`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storage {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl Memory {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    pub width: u32,
    pub height: u32,
    pub density_dpi: Option<u32>,
}

/// Gather everything for the dashboard. Blocking; takes a few hundred milliseconds.
pub fn device_info(serial: &str) -> Result<DeviceInfo> {
    let props = parse_getprop(&adb::run(Some(serial), &["shell", "getprop"])?);

    // One shell round-trip for the rest, split on a marker line.
    const SEP: &str = "@@SENTINEL@@";
    let script = format!(
        "dumpsys battery; echo {SEP}; df -k /data; echo {SEP}; cat /proc/meminfo; echo {SEP}; wm size; wm density; echo {SEP}; \
         nproc; for c in /sys/devices/system/cpu/cpu[0-9]*; do cat $c/cpufreq/cpuinfo_max_freq 2>/dev/null; done"
    );
    let out = adb::run(Some(serial), &["shell", &script]).context("reading device stats")?;
    let mut sections = out.split(SEP).map(str::trim);
    let mut next = || sections.next().unwrap_or("");
    let (battery, df, meminfo, wm, cpu) = (next(), next(), next(), next(), next());

    let prop = |key: &str| props.get(key).filter(|v| !v.is_empty()).cloned();
    Ok(DeviceInfo {
        serial: serial.to_string(),
        manufacturer: prop("ro.product.manufacturer").unwrap_or_default(),
        model: prop("ro.product.model").unwrap_or_default(),
        codename: prop("ro.product.device").unwrap_or_default(),
        android_version: prop("ro.build.version.release").unwrap_or_default(),
        sdk: prop("ro.build.version.sdk").and_then(|v| v.parse().ok()),
        security_patch: prop("ro.build.version.security_patch"),
        chipset: chipset(&prop, cpu),
        battery: parse_battery(battery),
        storage: parse_df(df),
        memory: parse_meminfo(meminfo),
        display: parse_wm(wm),
    })
}

fn chipset(prop: &impl Fn(&str) -> Option<String>, cpu: &str) -> Chipset {
    // `ro.soc.*` exists on Android 12+; older devices usually expose a chip name elsewhere.
    let model = prop("ro.soc.model")
        .or_else(|| prop("ro.hardware.chipname"))
        .or_else(|| prop("ro.board.platform"));
    let (cpu_cores, cpu_clusters) = parse_cpu(cpu);
    Chipset {
        manufacturer: prop("ro.soc.manufacturer"),
        name: model.as_deref().and_then(soc_name).map(str::to_string),
        model,
        platform: prop("ro.board.platform"),
        cpu_abi: prop("ro.product.cpu.abi"),
        cpu_cores,
        cpu_clusters,
    }
}

/// Marketing names for common SoC codes. Unknown codes are shown raw.
fn soc_name(code: &str) -> Option<&'static str> {
    Some(match code.to_ascii_lowercase().as_str() {
        // Samsung Exynos
        "s5e9945" => "Exynos 2400",
        "s5e9925" => "Exynos 2200",
        "s5e9840" | "exynos2100" => "Exynos 2100",
        "s5e8845" => "Exynos 1480",
        "s5e8835" => "Exynos 1380",
        "s5e8825" => "Exynos 1280",
        // Qualcomm Snapdragon
        "sm8750" => "Snapdragon 8 Elite",
        "sm8650" => "Snapdragon 8 Gen 3",
        "sm8550" => "Snapdragon 8 Gen 2",
        "sm8475" => "Snapdragon 8+ Gen 1",
        "sm8450" => "Snapdragon 8 Gen 1",
        "sm8350" => "Snapdragon 888",
        "sm8250" => "Snapdragon 865",
        "sm7675" => "Snapdragon 7+ Gen 3",
        "sm7550" => "Snapdragon 7 Gen 3",
        "sm6450" => "Snapdragon 6 Gen 1",
        // MediaTek Dimensity
        "mt6991" => "Dimensity 9400",
        "mt6989" => "Dimensity 9300",
        "mt6985" => "Dimensity 9200",
        "mt6983" => "Dimensity 9000",
        _ => return None,
    })
}

/// `nproc` on the first line, then each core's max frequency in kHz (one per line).
fn parse_cpu(out: &str) -> (Option<u32>, Vec<(u32, u32)>) {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let online = lines.next().and_then(|l| l.parse::<u32>().ok());
    let mut freqs_mhz: Vec<u32> = lines.filter_map(|l| l.parse::<u32>().ok()).map(|khz| khz / 1000).collect();
    freqs_mhz.sort_unstable_by(|a, b| b.cmp(a));
    let mut clusters: Vec<(u32, u32)> = Vec::new();
    for mhz in &freqs_mhz {
        match clusters.last_mut() {
            Some((count, f)) if f == mhz => *count += 1,
            _ => clusters.push((1, *mhz)),
        }
    }
    // Offline cores may hide their cpufreq entry, so take whichever count is larger.
    let cores = online.max((!freqs_mhz.is_empty()).then_some(freqs_mhz.len() as u32));
    (cores, clusters)
}

/// `getprop` prints lines like `[ro.product.model]: [SM-S721U]`.
fn parse_getprop(out: &str) -> HashMap<String, String> {
    out.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once("]: [")?;
            let key = key.strip_prefix('[')?;
            let value = value.trim_end().strip_suffix(']')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Parse the `Current Battery Service state` block of `dumpsys battery`. Vendors append lots of
/// extra lines, so only the first occurrence of each standard key is used.
fn parse_battery(out: &str) -> Option<Battery> {
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in out.lines() {
        // The standard fields are indented by exactly two spaces.
        let Some(rest) = line.strip_prefix("  ") else { continue };
        if rest.starts_with(' ') {
            continue;
        }
        if let Some((key, value)) = rest.split_once(": ") {
            fields.entry(key.trim()).or_insert(value.trim());
        }
    }

    let num = |key: &str| fields.get(key).and_then(|v| v.parse::<i64>().ok());
    let level = num("level")?;
    let scale = num("scale").filter(|s| *s > 0).unwrap_or(100);
    let powered = |key: &str| fields.get(key) == Some(&"true");

    Some(Battery {
        level: (level * 100 / scale).clamp(0, 100) as u8,
        // Values from android.os.BatteryManager.
        status: match num("status") {
            Some(2) => BatteryStatus::Charging,
            Some(3) => BatteryStatus::Discharging,
            Some(4) => BatteryStatus::NotCharging,
            Some(5) => BatteryStatus::Full,
            _ => BatteryStatus::Unknown,
        },
        health: match num("health") {
            Some(2) => BatteryHealth::Good,
            Some(3) => BatteryHealth::Overheat,
            Some(4) => BatteryHealth::Dead,
            Some(5) => BatteryHealth::OverVoltage,
            Some(6) => BatteryHealth::Failure,
            Some(7) => BatteryHealth::Cold,
            _ => BatteryHealth::Unknown,
        },
        temperature_c: num("temperature").map(|t| t as f32 / 10.0),
        voltage_mv: num("voltage").and_then(|v| u32::try_from(v).ok()),
        power_source: if powered("AC powered") {
            Some(PowerSource::Ac)
        } else if powered("USB powered") {
            Some(PowerSource::Usb)
        } else if powered("Wireless powered") {
            Some(PowerSource::Wireless)
        } else if powered("Dock powered") {
            Some(PowerSource::Dock)
        } else {
            None
        },
    })
}

/// `df -k`: header line, then `fs 1K-blocks used available use% mountpoint`.
fn parse_df(out: &str) -> Option<Storage> {
    let line = out.lines().nth(1)?;
    let cols: Vec<u64> = line.split_whitespace().skip(1).take(3).filter_map(|c| c.parse().ok()).collect();
    let [total, used, available] = cols[..] else { return None };
    Some(Storage {
        total_bytes: total * 1024,
        used_bytes: used * 1024,
        available_bytes: available * 1024,
    })
}

fn parse_meminfo(out: &str) -> Option<Memory> {
    let kb = |key: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
    };
    Some(Memory {
        total_bytes: kb("MemTotal")? * 1024,
        available_bytes: kb("MemAvailable").or_else(|| kb("MemFree"))? * 1024,
    })
}

/// `wm size` / `wm density`. An "Override" line (set by the user) wins over "Physical".
fn parse_wm(out: &str) -> Option<Display> {
    let value = |kind: &str| {
        let pick = |prefix: &str| {
            out.lines()
                .find(|l| l.starts_with(prefix) && l.contains(kind))
                .and_then(|l| l.rsplit(':').next())
                .map(str::trim)
        };
        pick("Override").or_else(|| pick("Physical"))
    };
    let (w, h) = value("size")?.split_once('x')?;
    Some(Display {
        width: w.parse().ok()?,
        height: h.parse().ok()?,
        density_dpi: value("density").and_then(|d| d.parse().ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATTERY: &str = "Current Battery Service state:
  AC powered: false
  USB powered: true
  Wireless powered: false
  Max charging current: 0
 Time when the latest updated value of the voltage was sent via battery changed broadcast: 0
  status: 2
  health: 2
  present: true
  level: 15
  scale: 100
  voltage: 3735
  temperature: 263
  technology: Li-ion
  current now: 383593
[BattActionChangedLogBuffer]
11-07 21:43:17.029  Sending ACTION_BATTERY_CHANGED: level:14, status:2
  level: 99
";

    #[test]
    fn parses_battery() {
        let b = parse_battery(BATTERY).unwrap();
        assert_eq!(b.level, 15);
        assert_eq!(b.status, BatteryStatus::Charging);
        assert_eq!(b.health, BatteryHealth::Good);
        assert_eq!(b.temperature_c, Some(26.3));
        assert_eq!(b.voltage_mv, Some(3735));
        assert_eq!(b.power_source, Some(PowerSource::Usb));
    }

    #[test]
    fn parses_getprop() {
        let p = parse_getprop("[ro.product.model]: [SM-S721U]\n[ro.build.version.release]: [17]\n[empty]: []\n");
        assert_eq!(p["ro.product.model"], "SM-S721U");
        assert_eq!(p["ro.build.version.release"], "17");
        assert_eq!(p["empty"], "");
    }

    #[test]
    fn parses_df() {
        let s = parse_df(
            "Filesystem       1K-blocks     Used Available Use% Mounted on\n\
             /dev/block/dm-71 109979628 84218012  25630544  77% /data/user/0\n",
        )
        .unwrap();
        assert_eq!(s.total_bytes, 109979628 * 1024);
        assert_eq!(s.used_bytes, 84218012 * 1024);
        assert_eq!(s.available_bytes, 25630544 * 1024);
    }

    #[test]
    fn parses_meminfo() {
        let m = parse_meminfo("MemTotal:        7397720 kB\nMemFree:          254296 kB\nMemAvailable:    2795768 kB\n").unwrap();
        assert_eq!(m.total_bytes, 7397720 * 1024);
        assert_eq!(m.available_bytes, 2795768 * 1024);
    }

    #[test]
    fn parses_cpu_clusters() {
        let (cores, clusters) =
            parse_cpu("10\n1959000\n1959000\n2592000\n1959000\n2900000\n2592000\n3110000\n1959000\n2592000\n2900000\n");
        assert_eq!(cores, Some(10));
        assert_eq!(clusters, vec![(1, 3110), (2, 2900), (3, 2592), (4, 1959)]);
    }

    #[test]
    fn names_known_socs() {
        assert_eq!(soc_name("s5e9945"), Some("Exynos 2400"));
        assert_eq!(soc_name("SM8650"), Some("Snapdragon 8 Gen 3"));
        assert_eq!(soc_name("erd9945"), None);
    }

    #[test]
    fn parses_wm() {
        let d = parse_wm("Physical size: 1080x2340\nPhysical density: 450\n").unwrap();
        assert_eq!((d.width, d.height, d.density_dpi), (1080, 2340, Some(450)));
        let d = parse_wm("Physical size: 1080x2340\nOverride size: 720x1560\nPhysical density: 450\n").unwrap();
        assert_eq!((d.width, d.height), (720, 1560));
    }
}
