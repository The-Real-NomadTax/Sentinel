//! Screenshots and screen recordings, saved on the computer (never on the phone).
//!
//! - Screenshots use Android's built-in `screencap` at full resolution, streamed straight back
//!   over adb, so nothing is written to the device.
//! - Recordings save the H.264 stream the agent already sends for mirroring into an MP4 file.

use std::{
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use mp4::{AvcConfig, FourCC, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};

use crate::adb;

/// `~/Pictures/Sentinel` (or the platform equivalent).
pub fn screenshots_dir() -> PathBuf {
    dirs::picture_dir().or_else(dirs::home_dir).unwrap_or_else(|| ".".into()).join("Sentinel")
}

/// `~/Videos/Sentinel` (or the platform equivalent).
pub fn recordings_dir() -> PathBuf {
    dirs::video_dir().or_else(dirs::home_dir).unwrap_or_else(|| ".".into()).join("Sentinel")
}

/// A timestamped file name like `Screenshot_SM-S721U_2026-11-07_23-40-12.png`.
pub fn timestamped_name(kind: &str, device: &str, ext: &str) -> String {
    let device: String = device
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    format!("{kind}_{device}_{stamp}.{ext}")
}

/// Capture the screen as PNG bytes at the device's full resolution.
pub fn screenshot_png(serial: &str) -> Result<Vec<u8>> {
    let output = adb::command(Some(serial))
        .args(["exec-out", "screencap", "-p"])
        .output()
        .context("failed to run adb screencap")?;
    if !output.status.success() {
        bail!("screencap failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    if !output.stdout.starts_with(b"\x89PNG\r\n\x1a\n") {
        bail!("screencap did not return a PNG image");
    }
    Ok(output.stdout)
}

/// Capture a screenshot into [`screenshots_dir`]. Returns the saved file.
pub fn save_screenshot(serial: &str, device_name: &str) -> Result<PathBuf> {
    let png = screenshot_png(serial)?;
    let dir = screenshots_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(timestamped_name("Screenshot", device_name, "png"));
    std::fs::write(&path, png).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingStatus {
    /// The file currently being written (a rotation starts a new numbered file).
    pub path: PathBuf,
    /// Video recorded so far, across all files.
    pub duration: Duration,
    pub frames: u64,
    /// True until the first key frame arrives; nothing has been written yet.
    pub waiting_for_key_frame: bool,
    pub error: Option<String>,
}

/// MP4 timescale for video samples (the common 90 kHz video clock).
const TIMESCALE: u32 = 90_000;
/// Duration given to the last frame, which has no successor to measure against.
const LAST_FRAME_US: u64 = 16_667;

/// Writes the mirroring stream to MP4. Fed by the mirror thread; see `Mirror::start_recording`.
pub struct Recorder {
    base: PathBuf,
    part: u32,
    files: Vec<PathBuf>,
    file: Option<Mp4File>,
    frames: u64,
    /// Duration of files already finished.
    finished_us: u64,
    error: Option<String>,
}

struct Mp4File {
    path: PathBuf,
    writer: Mp4Writer<BufWriter<File>>,
    first_pts: u64,
    /// Samples are written one behind, so each one's duration is known.
    pending: Option<(u64, Bytes, bool)>,
    last_duration_us: u64,
}

impl Recorder {
    /// Record to `path` (its directory is created if needed).
    pub fn new(path: PathBuf) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(Self { base: path, part: 1, files: Vec::new(), file: None, frames: 0, finished_us: 0, error: None })
    }

    pub fn status(&self) -> RecordingStatus {
        let current_us = self.file.as_ref().map_or(0, |f| {
            f.pending.as_ref().map_or(0, |(pts, _, _)| pts.saturating_sub(f.first_pts))
        });
        RecordingStatus {
            path: self.file.as_ref().map_or_else(|| self.part_path(), |f| f.path.clone()),
            duration: Duration::from_micros(self.finished_us + current_us),
            frames: self.frames,
            waiting_for_key_frame: self.file.is_none(),
            error: self.error.clone(),
        }
    }

    /// The stream restarted (rotation): close the current file; the next key frame starts a new one.
    pub(crate) fn on_session(&mut self) {
        if let Err(e) = self.close_file() {
            self.error = Some(format!("{e:#}"));
        }
    }

    /// One encoded frame (Annex B), with the latest codec config and frame size.
    pub(crate) fn on_frame(&mut self, data: &[u8], pts_us: u64, key_frame: bool, config: &[u8], size: (u32, u32)) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = self.write_frame(data, pts_us, key_frame, config, size) {
            self.error = Some(format!("{e:#}"));
            self.file = None;
        }
    }

    fn write_frame(&mut self, data: &[u8], pts_us: u64, key_frame: bool, config: &[u8], size: (u32, u32)) -> Result<()> {
        if self.file.is_none() {
            // A file must start with a key frame and its SPS/PPS.
            let Some((sps, pps)) = sps_pps(config).filter(|_| key_frame) else { return Ok(()) };
            self.file = Some(Mp4File::create(self.part_path(), size, sps, pps, pts_us)?);
        }
        let file = self.file.as_mut().unwrap();
        let sample = Bytes::from(annex_b_to_avcc(data));
        if let Some((prev_pts, prev, prev_key)) = file.pending.replace((pts_us, sample, key_frame)) {
            let duration = pts_us.saturating_sub(prev_pts).max(1);
            file.last_duration_us = duration;
            file.write(prev_pts, duration, prev, prev_key)?;
        }
        self.frames += 1;
        Ok(())
    }

    fn close_file(&mut self) -> Result<()> {
        let Some(mut file) = self.file.take() else { return Ok(()) };
        if let Some((pts, sample, key)) = file.pending.take() {
            let duration = if file.last_duration_us > 0 { file.last_duration_us } else { LAST_FRAME_US };
            file.write(pts, duration, sample, key)?;
            self.finished_us += pts.saturating_sub(file.first_pts) + duration;
        }
        file.writer.write_end().context("finishing MP4")?;
        use std::io::Write;
        file.writer.into_writer().flush().context("writing MP4")?;
        self.files.push(file.path);
        self.part += 1;
        Ok(())
    }

    /// Finish and close the recording. Returns the files written (empty if no key frame came).
    pub fn finish(mut self) -> Result<Vec<PathBuf>> {
        self.close_file()?;
        if let Some(err) = self.error.take() {
            bail!("recording failed: {err}");
        }
        Ok(std::mem::take(&mut self.files))
    }

    fn part_path(&self) -> PathBuf {
        if self.part == 1 {
            return self.base.clone();
        }
        let stem = self.base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.base.with_file_name(format!("{stem}_{}.mp4", self.part))
    }
}

impl Mp4File {
    fn create(path: PathBuf, size: (u32, u32), sps: &[u8], pps: &[u8], first_pts: u64) -> Result<Self> {
        let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        let brand = |s: &str| s.parse::<FourCC>().expect("valid fourcc");
        let mut writer = Mp4Writer::write_start(
            BufWriter::new(file),
            &Mp4Config {
                major_brand: brand("isom"),
                minor_version: 512,
                compatible_brands: vec![brand("isom"), brand("iso2"), brand("avc1"), brand("mp41")],
                timescale: 1000,
            },
        )
        .context("starting MP4")?;
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Video,
                timescale: TIMESCALE,
                language: "und".into(),
                media_conf: MediaConfig::AvcConfig(AvcConfig {
                    width: size.0.min(u16::MAX as u32) as u16,
                    height: size.1.min(u16::MAX as u32) as u16,
                    seq_param_set: sps.to_vec(),
                    pic_param_set: pps.to_vec(),
                }),
            })
            .context("adding MP4 video track")?;
        Ok(Self { path, writer, first_pts, pending: None, last_duration_us: 0 })
    }

    fn write(&mut self, pts_us: u64, duration_us: u64, bytes: Bytes, is_sync: bool) -> Result<()> {
        let to_ticks = |us: u64| us * TIMESCALE as u64 / 1_000_000;
        self.writer
            .write_sample(
                1,
                &Mp4Sample {
                    start_time: to_ticks(pts_us.saturating_sub(self.first_pts)),
                    duration: to_ticks(duration_us).clamp(1, u32::MAX as u64) as u32,
                    rendering_offset: 0,
                    is_sync,
                    bytes,
                },
            )
            .context("writing MP4 sample")
    }
}

/// Split an Annex B byte stream (start-code separated) into NAL units.
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |next| next - 3);
        // A 4-byte start code leaves an extra zero before the next unit.
        while end > start && data[end - 1] == 0 && n + 1 < starts.len() {
            end -= 1;
        }
        if end > start {
            units.push(&data[start..end]);
        }
    }
    units
}

fn nal_type(unit: &[u8]) -> u8 {
    unit[0] & 0x1f
}

/// SPS and PPS from a codec config packet.
fn sps_pps(config: &[u8]) -> Option<(&[u8], &[u8])> {
    let units = nal_units(config);
    let sps = units.iter().find(|u| nal_type(u) == 7)?;
    let pps = units.iter().find(|u| nal_type(u) == 8)?;
    Some((sps, pps))
}

/// MP4 stores NAL units length-prefixed; parameter sets live in the track header instead.
fn annex_b_to_avcc(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for unit in nal_units(data) {
        if matches!(nal_type(unit), 7..=9) {
            continue; // SPS, PPS, access unit delimiter
        }
        out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        out.extend_from_slice(unit);
    }
    out
}

/// Where a recording for `device_name` would go by default.
pub fn default_recording_path(device_name: &str) -> PathBuf {
    recordings_dir().join(timestamped_name("Recording", device_name, "mp4"))
}

/// True if `path` looks like a finished MP4 (has a `moov` box), for tests and sanity checks.
pub fn looks_like_mp4(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| b.len() > 8 && &b[4..8] == b"ftyp" && b.windows(4).any(|w| w == b"moov"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x20, 0xac, 0xb4];
    const PPS: &[u8] = &[0x68, 0xee, 0x0d, 0x8b];

    fn annex_b(units: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for (i, u) in units.iter().enumerate() {
            v.extend_from_slice(if i % 2 == 0 { &[0, 0, 0, 1] } else { &[0, 0, 1] });
            v.extend_from_slice(u);
        }
        v
    }

    #[test]
    fn splits_nal_units_with_both_start_codes() {
        let idr: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x10];
        let stream = annex_b(&[SPS, PPS, idr]);
        assert_eq!(nal_units(&stream), vec![SPS, PPS, idr]);
    }

    #[test]
    fn finds_sps_and_pps() {
        let both = annex_b(&[SPS, PPS]);
        let sps_only = annex_b(&[SPS]);
        assert_eq!(sps_pps(&both), Some((SPS, PPS)));
        assert_eq!(sps_pps(&sps_only), None);
    }

    #[test]
    fn converts_to_length_prefixed_without_parameter_sets() {
        let idr: &[u8] = &[0x65, 0xaa, 0xbb];
        let out = annex_b_to_avcc(&annex_b(&[SPS, PPS, idr]));
        assert_eq!(out, [0, 0, 0, 3, 0x65, 0xaa, 0xbb]);
    }

    #[test]
    fn writes_a_playable_mp4_and_splits_on_rotation() {
        let dir = std::env::temp_dir().join(format!("sentinel-rec-test-{}", std::process::id()));
        let path = dir.join("test.mp4");
        let config = annex_b(&[SPS, PPS]);
        let idr = annex_b(&[&[0x65, 0x88, 0x84]]);
        let p_frame = annex_b(&[&[0x41, 0x9a, 0x02]]);

        let mut rec = Recorder::new(path.clone()).unwrap();
        // Frames before the first key frame are skipped.
        rec.on_frame(&p_frame, 0, false, &config, (592, 1280));
        assert!(rec.status().waiting_for_key_frame);
        rec.on_frame(&idr, 1_000_000, true, &config, (592, 1280));
        rec.on_frame(&p_frame, 1_016_667, false, &config, (592, 1280));
        rec.on_frame(&p_frame, 1_033_333, false, &config, (592, 1280));
        assert_eq!(rec.status().frames, 3);
        rec.on_session(); // rotation
        rec.on_frame(&idr, 2_000_000, true, &config, (1280, 592));
        let files = rec.finish().unwrap();

        assert_eq!(files, vec![path.clone(), dir.join("test_2.mp4")]);
        assert!(files.iter().all(|f| looks_like_mp4(f)));
        let mut reader = mp4::Mp4Reader::read_header(
            std::io::BufReader::new(File::open(&path).unwrap()),
            std::fs::metadata(&path).unwrap().len(),
        )
        .unwrap();
        let track = reader.tracks().get(&1).unwrap();
        assert_eq!((track.width(), track.height()), (592, 1280));
        assert_eq!(reader.sample_count(1).unwrap(), 3);
        assert!(reader.read_sample(1, 1).unwrap().unwrap().is_sync);
        std::fs::remove_dir_all(&dir).ok();
    }
}
