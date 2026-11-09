//! Installed apps: listing them and exporting their APKs to this computer.
//!
//! Listing runs the agent once in `mode=apps`, because only the Android framework can read each
//! app's display name. If that fails, it falls back to `pm list packages`, which has no names.
//!
//! Exporting copies the APK files with `adb pull`. Apps installed as one APK are saved as
//! `Label_version.apk`; apps split into several APKs (most Play Store apps) are saved as a folder
//! `Label_version/` holding all of them, which [`crate::adb::install_apks`] can reinstall.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{adb, agent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub package: String,
    /// The name shown under the app icon. Falls back to the package name.
    pub label: String,
    pub version_name: Option<String>,
    pub version_code: Option<u64>,
    /// Preinstalled (including updated preinstalled apps).
    pub system: bool,
    pub enabled: bool,
    /// APK files on the device: the base APK first, then any splits.
    pub apk_paths: Vec<String>,
    /// Total size of the APK files in bytes (0 if unknown).
    pub size: u64,
}

impl AppInfo {
    /// Installed as a base APK plus split APKs.
    pub fn is_split(&self) -> bool {
        self.apk_paths.len() > 1
    }
}

/// List installed apps, sorted by name. Blocks for a second or two.
pub fn list_apps(serial: &str) -> Result<Vec<AppInfo>> {
    let mut apps = match list_with_agent(serial) {
        Ok(apps) if !apps.is_empty() => apps,
        agent_result => {
            let fallback = list_with_pm(serial);
            match (agent_result, fallback) {
                (_, Ok(apps)) => apps,
                (Err(agent_err), Err(pm_err)) => {
                    bail!("couldn't list apps: {agent_err:#}; pm fallback: {pm_err:#}")
                }
                (Ok(_), Err(pm_err)) => return Err(pm_err),
            }
        }
    };
    apps.sort_by_cached_key(|a| (a.label.to_lowercase(), a.package.clone()));
    Ok(apps)
}

fn list_with_agent(serial: &str) -> Result<Vec<AppInfo>> {
    agent::push_to(serial, agent::TASK_DEVICE_PATH)?;
    let cmd = format!(
        "CLASSPATH={path} app_process / {main} mode=apps cleanup={path}",
        path = agent::TASK_DEVICE_PATH,
        main = agent::MAIN_CLASS,
    );
    let output = adb::command(Some(serial)).args(["shell", &cmd]).output().context("failed to run the agent")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let apps = parse_agent_output(&stdout);
    if apps.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("the agent listed no apps: {}", stderr.trim());
    }
    Ok(apps)
}

fn parse_agent_output(out: &str) -> Vec<AppInfo> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
            let [_, package, label, version_name, version_code, system, enabled, paths, size] =
                f.as_slice().try_into().ok().filter(|f: &[&str; 9]| f[0] == "APP")?;
            Some(AppInfo {
                package: package.to_string(),
                label: if label.is_empty() { package.to_string() } else { label.to_string() },
                version_name: Some(version_name.to_string()).filter(|v| !v.is_empty()),
                version_code: version_code.parse().ok(),
                system: system == "1",
                enabled: enabled != "0",
                apk_paths: paths.split('|').filter(|p| !p.is_empty()).map(String::from).collect(),
                size: size.parse().unwrap_or(0),
            })
        })
        .collect()
}

/// Names-less listing for devices where the agent can't run.
fn list_with_pm(serial: &str) -> Result<Vec<AppInfo>> {
    let all = adb::run(Some(serial), &["shell", "pm list packages -f --show-versioncode"])?;
    let system: HashSet<String> =
        parse_package_names(&adb::run(Some(serial), &["shell", "pm list packages -s"])?).collect();
    let disabled: HashSet<String> =
        parse_package_names(&adb::run(Some(serial), &["shell", "pm list packages -d"])?).collect();
    Ok(parse_pm_list(&all)
        .into_iter()
        .map(|mut app| {
            app.system = system.contains(&app.package);
            app.enabled = !disabled.contains(&app.package);
            app
        })
        .collect())
}

fn parse_package_names(out: &str) -> impl Iterator<Item = String> + '_ {
    out.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(String::from)
}

/// Parse `package:/data/app/…/base.apk=com.example versionCode:12` lines.
fn parse_pm_list(out: &str) -> Vec<AppInfo> {
    out.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("package:")?;
            let (rest, version_code) = match rest.rsplit_once(" versionCode:") {
                Some((rest, code)) => (rest, code.trim().parse().ok()),
                None => (rest, None),
            };
            // The path may itself contain '=', the package name never does.
            let (path, package) = rest.rsplit_once('=')?;
            Some(AppInfo {
                package: package.to_string(),
                label: package.to_string(),
                version_name: None,
                version_code,
                system: false,
                enabled: true,
                apk_paths: vec![path.to_string()],
                size: 0,
            })
        })
        .collect()
}

/// The APK files of an installed package, as reported right now (`pm path`).
pub fn apk_paths(serial: &str, package: &str) -> Result<Vec<String>> {
    let out = adb::run(Some(serial), &["shell", &format!("pm path {package}")])?;
    let paths: Vec<String> = parse_package_names(&out).collect();
    if paths.is_empty() {
        bail!("{package} is not installed");
    }
    Ok(paths)
}

/// `~/Downloads/Sentinel` (or the platform equivalent): where exported APKs go.
pub fn exports_dir() -> PathBuf {
    dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| ".".into()).join("Sentinel")
}

/// Copy an app's APK(s) into `dir`. Returns the saved `.apk` file, or the folder for split apps.
/// Never overwrites: an existing export of the same version gets a numbered name.
pub fn export_app(serial: &str, app: &AppInfo, dir: &Path) -> Result<PathBuf> {
    // Ask again rather than trusting the list: the app may have been updated since.
    let paths = apk_paths(serial, &app.package)?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let mut stem = safe_file_name(&app.label);
    if let Some(version) = &app.version_name {
        stem = format!("{stem}_{}", safe_file_name(version));
    }
    let split = paths.len() > 1;
    let target = unused_path(dir, &stem, if split { "" } else { ".apk" });
    // Pull into a temporary name first, so a failed export never leaves a half-written APK
    // that looks finished.
    let partial =
        target.with_file_name(format!(".{}.partial", target.file_name().and_then(|n| n.to_str()).unwrap_or("export")));
    remove_any(&partial);

    let result = (|| {
        if split {
            std::fs::create_dir(&partial).with_context(|| format!("creating {}", partial.display()))?;
        }
        let partial_str = partial.to_str().context("export path is not valid UTF-8")?;
        let mut args: Vec<&str> = vec!["pull"];
        args.extend(paths.iter().map(String::as_str));
        args.push(partial_str);
        adb::run(Some(serial), &args).context("adb pull failed")?;

        let files: Vec<PathBuf> = if split {
            paths.iter().map(|p| partial.join(p.rsplit('/').next().unwrap_or(p))).collect()
        } else {
            vec![partial.clone()]
        };
        for file in &files {
            if !looks_like_apk(file) {
                bail!("{} is not a valid APK", file.display());
            }
        }
        std::fs::rename(&partial, &target).with_context(|| format!("saving {}", target.display()))?;
        Ok(target.clone())
    })();
    if result.is_err() {
        remove_any(&partial);
    }
    result
}

fn remove_any(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir_all(path);
}

/// APKs are zip files.
fn looks_like_apk(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"PK\x03\x04"
}

/// `dir/stem.ext`, or `dir/stem (2).ext`, `dir/stem (3).ext`, … if taken.
fn unused_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}{ext}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

/// Make an app name safe to use as a file name on any desktop OS.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c if c.is_whitespace() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == '_');
    let short: String = trimmed.chars().take(80).collect();
    if short.is_empty() { "app".into() } else { short }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_lines() {
        let out = "[agent] noise\n\
            APP\tcom.example\tExample App\t1.2.3\t42\t0\t1\t/data/app/x/base.apk|/data/app/x/split_config.en.apk\t1234\n\
            APP\tcom.android.settings\t\t\t\t1\t0\t/system/priv-app/Settings/Settings.apk\t99\r\n\
            APP\tbroken\tline\n";
        let apps = parse_agent_output(out);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].label, "Example App");
        assert_eq!(apps[0].version_name.as_deref(), Some("1.2.3"));
        assert_eq!(apps[0].version_code, Some(42));
        assert!(!apps[0].system && apps[0].enabled && apps[0].is_split());
        assert_eq!(apps[0].size, 1234);
        assert_eq!(apps[1].label, "com.android.settings");
        assert_eq!(apps[1].version_name, None);
        assert!(apps[1].system && !apps[1].enabled && !apps[1].is_split());
    }

    #[test]
    fn parses_pm_list() {
        let out = "package:/data/app/~~a==/com.ex-b==/base.apk=com.ex versionCode:7\n\
                   package:/system/app/Odd=Name/Odd.apk=com.odd versionCode:0\n";
        let apps = parse_pm_list(out);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].package, "com.ex");
        assert_eq!(apps[0].apk_paths, ["/data/app/~~a==/com.ex-b==/base.apk"]);
        assert_eq!(apps[0].version_code, Some(7));
        assert_eq!(apps[1].package, "com.odd");
        assert_eq!(apps[1].apk_paths, ["/system/app/Odd=Name/Odd.apk"]);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_file_name("YouTube ReVanced"), "YouTube_ReVanced");
        assert_eq!(safe_file_name("a/b: c?"), "a_b__c");
        assert_eq!(safe_file_name("..."), "app");
        assert_eq!(safe_file_name("日本語"), "日本語");
    }

    #[test]
    fn numbered_names_avoid_collisions() {
        let dir = std::env::temp_dir().join(format!("sentinel-apps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unused_path(&dir, "A_1", ".apk"), dir.join("A_1.apk"));
        std::fs::write(dir.join("A_1.apk"), b"x").unwrap();
        assert_eq!(unused_path(&dir, "A_1", ".apk"), dir.join("A_1 (2).apk"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
