//! Install an APK on the first ready device: `cargo run -p sentinel-core --example install -- app.apk`
use sentinel_core::adb;

fn main() -> anyhow::Result<()> {
    let apk = std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("usage: install <file.apk>"))?;
    let device = adb::list_devices()?.into_iter().find(|d| d.is_ready()).ok_or_else(|| anyhow::anyhow!("no ready device"))?;
    println!("installing {apk} on {}…", device.display_name());
    println!("{}", adb::install_apk(&device.serial, apk.as_ref())?);
    Ok(())
}
