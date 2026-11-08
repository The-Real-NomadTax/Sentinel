//! Take a screenshot and a short recording of the first ready device, saved on this computer.
//! Presses Recents and Back during the recording so there is motion.
//! `cargo run -p sentinel-core --example media_probe -- <output dir>`
use std::{path::PathBuf, thread::sleep, time::Duration};
use sentinel_core::{adb, control::keycode, media, Mirror, MirrorConfig, MirrorStatus};

fn main() -> anyhow::Result<()> {
    let out: PathBuf = std::env::args().nth(1).map(Into::into).unwrap_or_else(std::env::temp_dir);
    let device = adb::list_devices()?.into_iter().find(|d| d.is_ready()).ok_or_else(|| anyhow::anyhow!("no device"))?;

    let png = media::screenshot_png(&device.serial)?;
    let shot = out.join("probe-screenshot.png");
    std::fs::write(&shot, &png)?;
    println!("screenshot: {} ({} bytes)", shot.display(), png.len());

    let mirror = Mirror::start(&device.serial, MirrorConfig::default());
    for _ in 0..100 {
        if matches!(mirror.status(), MirrorStatus::Streaming { .. } | MirrorStatus::Failed(_)) { break; }
        sleep(Duration::from_millis(100));
    }
    println!("status: {:?}", mirror.status());
    let started = std::time::Instant::now();
    mirror.start_recording(out.join("probe-recording.mp4"))?;
    sleep(Duration::from_millis(400));
    println!("after keyframe request: {:?}", mirror.recording_status());
    let ctl = mirror.controller().unwrap();
    sleep(Duration::from_secs(1));
    ctl.press_key(keycode::APP_SWITCH, 0);
    sleep(Duration::from_secs(2));
    ctl.back();
    sleep(Duration::from_secs(2));
    println!("before stop: {:?} (wall {:.1}s)", mirror.recording_status(), started.elapsed().as_secs_f32());
    for f in mirror.stop_recording().unwrap()? {
        println!("saved {} ({} bytes, valid mp4: {})", f.display(), std::fs::metadata(&f)?.len(), media::looks_like_mp4(&f));
    }
    Ok(())
}
