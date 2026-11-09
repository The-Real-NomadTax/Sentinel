//! Headless check of app listing and APK export.
//! Usage: apps_probe [package-to-export ...] (exports into a temp folder)
//!        apps_probe --install <exported split-app folder>

use std::time::Instant;

use sentinel_core::{adb, apps};

fn main() -> anyhow::Result<()> {
    let device = adb::list_devices()?.into_iter().find(|d| d.is_ready()).expect("no ready device");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, dir] = args.as_slice()
        && flag == "--install"
    {
        let apks = adb::apks_in_dir(dir.as_ref())?;
        println!("installing {apks:?}");
        println!("{}", adb::install_apks(&device.serial, &apks)?);
        return Ok(());
    }
    let start = Instant::now();
    let list = apps::list_apps(&device.serial)?;
    let user = list.iter().filter(|a| !a.system).count();
    println!(
        "{} apps ({user} user, {} split) in {:?}",
        list.len(),
        list.iter().filter(|a| a.is_split()).count(),
        start.elapsed()
    );
    for app in list.iter().filter(|a| !a.system).take(5) {
        println!(
            "  {} [{}] {:?} {} files {} bytes",
            app.label,
            app.package,
            app.version_name,
            app.apk_paths.len(),
            app.size
        );
    }
    let out = std::env::temp_dir().join("sentinel-apps-probe");
    for package in std::env::args().skip(1) {
        let app = list.iter().find(|a| a.package == package).expect("package not in list");
        let start = Instant::now();
        let path = apps::export_app(&device.serial, app, &out)?;
        println!("exported {package} -> {} in {:?}", path.display(), start.elapsed());
    }
    Ok(())
}
