//! Screen mirroring.
//!
//! Sentinel's own Kotlin agent (see [`crate::agent`]) is pushed to the phone, started with
//! `app_process`, and streams H.264 over an `adb forward` socket; a second socket carries input.
//! We decode the stream with OpenH264 on a background thread and keep only the latest frame, so
//! a slow frontend never builds up lag.

use std::{
    io::{BufRead, BufReader, Read},
    net::{Shutdown, TcpStream},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use openh264::{decoder::Decoder, formats::YUVSource};

use crate::{
    adb, agent,
    control::Controller,
    media::{Recorder, RecordingStatus},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8,
    /// What GPUI (and many GPU texture uploads) want.
    Bgra8,
}

#[derive(Clone, Debug)]
pub struct MirrorConfig {
    /// Longest side of the video in pixels (0 = native resolution).
    pub max_size: u32,
    pub bit_rate: u32,
    pub max_fps: u32,
    pub pixel_format: PixelFormat,
    /// Accept input (touch, keys) from the computer. See [`Mirror::controller`].
    pub control: bool,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            max_size: 1280,
            bit_rate: 8_000_000,
            max_fps: 60,
            pixel_format: PixelFormat::Rgba8,
            control: true,
        }
    }
}

/// One decoded video frame. `data` is tightly packed, 4 bytes per pixel.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    /// Increments for every decoded frame in this session.
    pub index: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorStatus {
    Starting,
    Streaming { device_name: String, width: u32, height: u32 },
    Stopped,
    Failed(String),
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    latest: Mutex<Option<Frame>>,
    status: Mutex<Option<MirrorStatus>>,
    frames_decoded: AtomicU64,
    /// Kept so `stop()` can unblock the reader thread.
    socket: Mutex<Option<TcpStream>>,
    control_socket: Mutex<Option<TcpStream>>,
    controller: Mutex<Option<Controller>>,
    recorder: Mutex<Option<Recorder>>,
    /// A recording finished because the session ended (e.g. the phone was unplugged),
    /// waiting to be picked up by `stop_recording`.
    finished_recording: Mutex<Option<Result<Vec<PathBuf>>>>,
    server: Mutex<Option<Child>>,
}

impl Shared {
    fn set_status(&self, status: MirrorStatus) {
        *self.status.lock().unwrap() = Some(status);
    }
}

/// A running mirroring session. Dropping it stops the stream and cleans up.
pub struct Mirror {
    serial: String,
    shared: Arc<Shared>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Mirror {
    /// Start mirroring `serial`. Returns immediately; watch [`Mirror::status`] for progress.
    pub fn start(serial: impl Into<String>, config: MirrorConfig) -> Self {
        let serial = serial.into();
        let shared = Arc::new(Shared::default());
        shared.set_status(MirrorStatus::Starting);

        let thread = {
            let serial = serial.clone();
            let shared = shared.clone();
            thread::Builder::new()
                .name("sentinel-mirror".into())
                .spawn(move || {
                    let result = run_session(&serial, &config, &shared);
                    let final_status = match result {
                        Ok(()) => MirrorStatus::Stopped,
                        Err(_) if shared.stop.load(Ordering::Relaxed) => MirrorStatus::Stopped,
                        Err(err) => MirrorStatus::Failed(format!("{err:#}")),
                    };
                    shared.set_status(final_status);
                    cleanup(&shared);
                })
                .expect("failed to spawn mirror thread")
        };

        Self { serial, shared, thread: Some(thread) }
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn status(&self) -> MirrorStatus {
        self.shared.status.lock().unwrap().clone().unwrap_or(MirrorStatus::Starting)
    }

    /// Take the newest frame decoded since the last call, if any.
    pub fn take_frame(&self) -> Option<Frame> {
        self.shared.latest.lock().unwrap().take()
    }

    /// Send input to the device. `None` until connected, or if `control` was disabled.
    pub fn controller(&self) -> Option<Controller> {
        self.shared.controller.lock().unwrap().clone()
    }

    /// Start saving the stream to an MP4 file on this computer. Recording begins at the next
    /// key frame (one is requested right away). A rotation continues in `<name>_2.mp4`, etc.
    pub fn start_recording(&self, path: PathBuf) -> Result<()> {
        let mut recorder = self.shared.recorder.lock().unwrap();
        if recorder.is_some() {
            bail!("already recording");
        }
        *recorder = Some(Recorder::new(path)?);
        drop(recorder);
        if let Some(ctl) = self.controller() {
            ctl.request_key_frame();
        }
        Ok(())
    }

    /// Stop recording and finish the file(s). Also returns a recording that ended on its own
    /// because the session stopped. `None` if there is nothing to report.
    pub fn stop_recording(&self) -> Option<Result<Vec<PathBuf>>> {
        if let Some(recorder) = self.shared.recorder.lock().unwrap().take() {
            return Some(recorder.finish());
        }
        self.shared.finished_recording.lock().unwrap().take()
    }

    pub fn recording_status(&self) -> Option<RecordingStatus> {
        self.shared.recorder.lock().unwrap().as_ref().map(Recorder::status)
    }

    pub fn frames_decoded(&self) -> u64 {
        self.shared.frames_decoded.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        cleanup(&self.shared);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.stop();
    }
}

fn cleanup(shared: &Shared) {
    // Finish any recording so the file is playable even if the frontend never stopped it.
    if let Some(recorder) = shared.recorder.lock().unwrap().take() {
        *shared.finished_recording.lock().unwrap() = Some(recorder.finish());
    }
    shared.controller.lock().unwrap().take();
    for socket in [&shared.socket, &shared.control_socket] {
        if let Some(socket) = socket.lock().unwrap().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
    if let Some(mut child) = shared.server.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn run_session(serial: &str, config: &MirrorConfig, shared: &Shared) -> Result<()> {
    agent::push(serial)?;

    // A random socket name lets several sessions coexist on one device.
    let socket_name = format!("sentinel_{:08x}", random_id());
    let port = adb::run(Some(serial), &["forward", "tcp:0", &format!("localabstract:{socket_name}")])?
        .trim()
        .parse::<u16>()
        .context("could not parse port from `adb forward`")?;
    let _forward = ForwardGuard { serial: serial.to_string(), port };

    let agent_cmd = format!(
        "CLASSPATH={path} app_process / {main} socket={socket_name} cleanup={path} \
         max_size={} bit_rate={} max_fps={}",
        config.max_size,
        config.bit_rate,
        config.max_fps,
        path = agent::DEVICE_PATH,
        main = agent::MAIN_CLASS,
    );
    let mut child = adb::command(Some(serial))
        .args(["shell", &agent_cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start the agent")?;

    // Collect the agent's output so failures show a useful message.
    let agent_log = Arc::new(Mutex::new(String::new()));
    for pipe in [
        child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let log = agent_log.clone();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut log = log.lock().unwrap();
                log.push_str(&line);
                log.push('\n');
            }
        });
    }
    *shared.server.lock().unwrap() = Some(child);

    let (mut stream, device_name) = connect(port, config.control, shared).with_context(|| {
        let log = agent_log.lock().unwrap();
        format!("could not connect to the agent. Agent output:\n{}", log.trim())
    })?;

    let result = stream_video(&mut stream, &device_name, config, shared);
    if result.is_err() && !shared.stop.load(Ordering::Relaxed) {
        let log = agent_log.lock().unwrap();
        if !log.trim().is_empty() {
            return result.with_context(|| format!("agent output:\n{}", log.trim()));
        }
    }
    result
}

/// Connect the video socket, read the handshake, then connect the control socket.
///
/// `adb forward` accepts connections before the agent is listening and then closes them, so we
/// retry until the handshake arrives.
fn connect(port: u16, control: bool, shared: &Shared) -> Result<(TcpStream, String)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            bail!("stopped");
        }
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let mut magic = [0u8; 4];
            if stream.read_exact(&mut magic).is_ok() {
                if &magic != b"SNTL" {
                    bail!("unexpected handshake {magic:02x?}");
                }
                let device_name = read_handshake(&mut stream)?;
                stream.set_nodelay(true).ok();
                *shared.socket.lock().unwrap() = Some(stream.try_clone()?);
                if control {
                    let control = TcpStream::connect(("127.0.0.1", port)).context("connecting control socket")?;
                    control.set_nodelay(true).ok();
                    *shared.control_socket.lock().unwrap() = Some(control.try_clone()?);
                    *shared.controller.lock().unwrap() = Some(Controller::spawn(control)?);
                }
                return Ok((stream, device_name));
            }
        }
        // If the agent already exited there's no point waiting.
        if let Some(child) = shared.server.lock().unwrap().as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            bail!("the agent exited ({status})");
        }
        if Instant::now() > deadline {
            bail!("timed out");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// After the magic: version u8, name length u16, name.
fn read_handshake(stream: &mut TcpStream) -> Result<String> {
    let mut version = [0u8; 1];
    stream.read_exact(&mut version)?;
    if version[0] != agent::PROTOCOL_VERSION {
        bail!("agent speaks protocol {}, expected {}", version[0], agent::PROTOCOL_VERSION);
    }
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    let mut name = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut name)?;
    Ok(String::from_utf8_lossy(&name).into_owned())
}

/// Packet header: flags+pts u64 (bit 63 session, bit 62 codec config, bit 61 key frame), length u32.
const FLAG_SESSION: u64 = 1 << 63;
const FLAG_CONFIG: u64 = 1 << 62;
const FLAG_KEY_FRAME: u64 = 1 << 61;
const PTS_MASK: u64 = (1 << 61) - 1;

fn stream_video(stream: &mut TcpStream, device_name: &str, config: &MirrorConfig, shared: &Shared) -> Result<()> {
    let mut decoder = Decoder::new().context("failed to create H.264 decoder")?;
    let mut config_packet: Vec<u8> = Vec::new();
    let mut packet = Vec::new();
    let mut index = 0u64;
    let mut last_size = (0, 0);
    // For recording: the current stream's SPS/PPS and frame size.
    let mut stream_config: Vec<u8> = Vec::new();
    let mut stream_size = (0u32, 0u32);

    while !shared.stop.load(Ordering::Relaxed) {
        let mut header = [0u8; 12];
        stream.read_exact(&mut header).context("stream ended")?;
        let flags = u64::from_be_bytes(header[0..8].try_into().unwrap());
        let len = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
        if len > 64 * 1024 * 1024 {
            bail!("implausible packet size {len}; stream out of sync");
        }
        packet.resize(len, 0);
        stream.read_exact(&mut packet).context("stream ended mid-packet")?;

        if flags & FLAG_SESSION != 0 {
            // New capture session (e.g. after rotation); payload is the new frame size.
            // A fresh decoder avoids artifacts from the old stream.
            if packet.len() >= 8 {
                stream_size = (
                    u32::from_be_bytes(packet[0..4].try_into().unwrap()),
                    u32::from_be_bytes(packet[4..8].try_into().unwrap()),
                );
            }
            decoder = Decoder::new().context("failed to create H.264 decoder")?;
            config_packet.clear();
            stream_config.clear();
            if let Some(rec) = shared.recorder.lock().unwrap().as_mut() {
                rec.on_session();
            }
            continue;
        }
        if flags & FLAG_CONFIG != 0 {
            // SPS/PPS: feed it together with the next frame.
            config_packet = packet.clone();
            stream_config = packet.clone();
            continue;
        }
        if let Some(rec) = shared.recorder.lock().unwrap().as_mut() {
            rec.on_frame(&packet, flags & PTS_MASK, flags & FLAG_KEY_FRAME != 0, &stream_config, stream_size);
        }
        let data: &[u8] = if config_packet.is_empty() {
            &packet
        } else {
            config_packet.extend_from_slice(&packet);
            &config_packet
        };

        let frame = match decoder.decode(data) {
            Ok(Some(yuv)) => {
                let (w, h) = yuv.dimensions();
                let mut pixels = vec![0u8; w * h * 4];
                yuv.write_rgba8(&mut pixels);
                if config.pixel_format == PixelFormat::Bgra8 {
                    for px in pixels.as_chunks_mut::<4>().0 {
                        px.swap(0, 2);
                    }
                }
                Some(Frame {
                    width: w as u32,
                    height: h as u32,
                    format: config.pixel_format,
                    data: pixels,
                    index,
                })
            }
            // `None` = decoder needs more data. Errors on a corrupt frame shouldn't kill the
            // session; the next keyframe recovers.
            Ok(None) | Err(_) => None,
        };
        config_packet.clear();

        if let Some(frame) = frame {
            index += 1;
            shared.frames_decoded.store(index, Ordering::Relaxed);
            if (frame.width, frame.height) != last_size {
                last_size = (frame.width, frame.height);
                shared.set_status(MirrorStatus::Streaming {
                    device_name: device_name.to_string(),
                    width: frame.width,
                    height: frame.height,
                });
            }
            *shared.latest.lock().unwrap() = Some(frame);
        }
    }
    Ok(())
}

struct ForwardGuard {
    serial: String,
    port: u16,
}

impl Drop for ForwardGuard {
    fn drop(&mut self) {
        let _ = adb::run(Some(&self.serial), &["forward", "--remove", &format!("tcp:{}", self.port)]);
    }
}

fn random_id() -> u32 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    (nanos as u32) ^ std::process::id().rotate_left(16)
}
