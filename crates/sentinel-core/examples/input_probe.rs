//! Check that input reaches the device: press Home, Recents, then Back, printing the focused
//! window after each. `cargo run -p sentinel-core --example input_probe`
use std::{thread::sleep, time::Duration};
use sentinel_core::{adb, control::keycode, Mirror, MirrorConfig, MirrorStatus};

fn focused(serial: &str) -> String {
    let out = adb::command(Some(serial)).args(["shell", "dumpsys window | grep mCurrentFocus | grep -v null | head -1"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn main() -> anyhow::Result<()> {
    let device = adb::list_devices()?.into_iter().find(|d| d.is_ready()).ok_or_else(|| anyhow::anyhow!("no device"))?;
    let mirror = Mirror::start(&device.serial, MirrorConfig::default());
    for _ in 0..100 {
        if matches!(mirror.status(), MirrorStatus::Streaming { .. } | MirrorStatus::Failed(_)) { break; }
        sleep(Duration::from_millis(100));
    }
    println!("status: {:?}", mirror.status());
    let ctl = mirror.controller().ok_or_else(|| anyhow::anyhow!("no controller"))?;
    println!("before:  {}", focused(&device.serial));
    ctl.press_key(keycode::HOME, 0); sleep(Duration::from_millis(1200));
    println!("home:    {}", focused(&device.serial));
    ctl.press_key(keycode::APP_SWITCH, 0); sleep(Duration::from_millis(1200));
    println!("recents: {}", focused(&device.serial));
    ctl.back(); sleep(Duration::from_millis(1200));
    println!("back:    {}", focused(&device.serial));
    Ok(())
}
