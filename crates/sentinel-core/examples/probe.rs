//! Quick headless check: list devices and mirror the first ready one for a few seconds.
//! `cargo run -p sentinel-core --example probe`
use std::time::{Duration, Instant};
use sentinel_core::{adb, Mirror, MirrorConfig};

fn main() -> anyhow::Result<()> {
    let devices = adb::list_devices()?;
    println!("devices: {devices:#?}");
    let device = devices.iter().find(|d| d.is_ready()).ok_or_else(|| anyhow::anyhow!("no ready device"))?;
    let mirror = Mirror::start(&device.serial, MirrorConfig::default());
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < Duration::from_secs(13) {
        let status = mirror.status();
        if Some(&status) != last.as_ref() { println!("{:>6.2}s {status:?}", start.elapsed().as_secs_f32()); last = Some(status); }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("decoded {} frames in 13s", mirror.frames_decoded());
    Ok(())
}
