//! Screen mirroring.
//!
//! We reuse the small Java server that ships with scrcpy (`scrcpy-server`): it is pushed to the
//! phone, started with `app_process`, and streams H.264 over an `adb forward` socket. We decode
//! the stream with OpenH264 on a background thread and keep only the latest frame, so a slow
//! frontend never builds up lag.

use std::{
    io::{BufRead, BufReader, Read},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use openh264::{decoder::Decoder, formats::YUVSource};

use crate::adb;

const DEVICE_SERVER_PATH: &str = "/data/local/tmp/sentinel-scrcpy-server.jar";

/// Where to look for `scrcpy-server` if none is configured.
const SERVER_SEARCH_PATHS: &[&str] = &[
    "/usr/share/scrcpy/scrcpy-server",
    "/usr/local/share/scrcpy/scrcpy-server",
    "/opt/homebrew/share/scrcpy/scrcpy-server",
];

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
    /// Path to `scrcpy-server`. Falls back to `SCRCPY_SERVER_PATH`, then common install paths.
    pub server_path: Option<PathBuf>,
    /// Version string the server expects (must match the server file exactly).
    /// Falls back to parsing `scrcpy --version`.
    pub server_version: Option<String>,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            max_size: 1280,
            bit_rate: 8_000_000,
            max_fps: 60,
            pixel_format: PixelFormat::Rgba8,
            server_path: None,
            server_version: None,
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
    if let Some(socket) = shared.socket.lock().unwrap().take() {
        let _ = socket.shutdown(Shutdown::Both);
    }
    if let Some(mut child) = shared.server.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn run_session(serial: &str, config: &MirrorConfig, shared: &Shared) -> Result<()> {
    let server_path = find_server(config)?;
    let version = match &config.server_version {
        Some(v) => v.clone(),
        None => detect_server_version()?,
    };

    adb::run(Some(serial), &["push", path_str(&server_path)?, DEVICE_SERVER_PATH])
        .context("failed to push scrcpy-server to the device")?;

    // A random id lets several sessions (or a real scrcpy) coexist on one device.
    let scid = random_scid();
    let socket_name = format!("scrcpy_{scid:08x}");
    let port = adb::run(Some(serial), &["forward", "tcp:0", &format!("localabstract:{socket_name}")])?
        .trim()
        .parse::<u16>()
        .context("could not parse port from `adb forward`")?;
    let _forward = ForwardGuard { serial: serial.to_string(), port };

    let server_cmd = format!(
        "CLASSPATH={DEVICE_SERVER_PATH} app_process / com.genymobile.scrcpy.Server {version} \
         scid={scid:08x} log_level=info tunnel_forward=true audio=false control=false \
         cleanup=true video_codec=h264 max_size={} video_bit_rate={} max_fps={}",
        config.max_size, config.bit_rate, config.max_fps
    );
    let mut child = adb::command(Some(serial))
        .args(["shell", &server_cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start scrcpy-server")?;

    // Collect server output so failures show a useful message.
    let server_log = Arc::new(Mutex::new(String::new()));
    for pipe in [
        child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let log = server_log.clone();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut log = log.lock().unwrap();
                log.push_str(&line);
                log.push('\n');
            }
        });
    }
    *shared.server.lock().unwrap() = Some(child);

    let mut stream = connect(port, shared).with_context(|| {
        let log = server_log.lock().unwrap();
        format!("could not connect to scrcpy-server. Server output:\n{}", log.trim())
    })?;

    let result = stream_video(&mut stream, config, &version, shared);
    if result.is_err() && !shared.stop.load(Ordering::Relaxed) {
        let log = server_log.lock().unwrap();
        if !log.trim().is_empty() {
            return result.with_context(|| format!("server output:\n{}", log.trim()));
        }
    }
    result
}

/// `adb forward` succeeds before the server is listening, so a connection can be accepted and
/// immediately closed. The server writes one "dummy" byte once it is really ready.
fn connect(port: u16, shared: &Shared) -> Result<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            bail!("stopped");
        }
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let mut dummy = [0u8; 1];
            if stream.read_exact(&mut dummy).is_ok() {
                stream.set_nodelay(true).ok();
                *shared.socket.lock().unwrap() = Some(stream.try_clone()?);
                return Ok(stream);
            }
        }
        // If the server already exited there's no point waiting.
        if let Some(child) = shared.server.lock().unwrap().as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            bail!("scrcpy-server exited ({status})");
        }
        if Instant::now() > deadline {
            bail!("timed out");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Wire format of the video socket. It changed in scrcpy 4.0.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Protocol {
    /// Before 4.0: 12-byte codec header (id, width, height); config flag is bit 63.
    Legacy,
    /// 4.0 and later: 4-byte codec id; 12-byte "session" packets (bit 63 set) carry the size;
    /// in media headers bit 62 = config, bit 61 = key frame.
    V4,
}

impl Protocol {
    fn for_version(version: &str) -> Self {
        let major = version.split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
        if major >= 4 { Protocol::V4 } else { Protocol::Legacy }
    }
}

fn stream_video(stream: &mut TcpStream, config: &MirrorConfig, version: &str, shared: &Shared) -> Result<()> {
    let protocol = Protocol::for_version(version);

    // Device meta: 64-byte NUL-padded device name.
    let mut name = [0u8; 64];
    stream.read_exact(&mut name).context("reading device name")?;
    let device_name = String::from_utf8_lossy(&name).trim_end_matches('\0').to_string();

    let mut codec = [0u8; 4];
    stream.read_exact(&mut codec).context("reading codec header")?;
    if &codec != b"h264" {
        bail!("unexpected codec {:?}", String::from_utf8_lossy(&codec));
    }
    if protocol == Protocol::Legacy {
        let mut size = [0u8; 8]; // initial width + height; we take the size from decoded frames
        stream.read_exact(&mut size)?;
    }

    let mut decoder = Decoder::new().context("failed to create H.264 decoder")?;
    let mut config_packet: Vec<u8> = Vec::new();
    let mut packet = Vec::new();
    let mut index = 0u64;
    let mut last_size = (0, 0);

    while !shared.stop.load(Ordering::Relaxed) {
        let mut header = [0u8; 12];
        stream.read_exact(&mut header).context("stream ended")?;
        let flags = u64::from_be_bytes(header[0..8].try_into().unwrap());

        let is_config = match protocol {
            Protocol::Legacy => flags & (1 << 63) != 0,
            Protocol::V4 => {
                if flags & (1 << 63) != 0 {
                    // Session packet (new capture session, e.g. after rotation). No payload.
                    // A fresh decoder avoids artifacts from the old stream's state.
                    decoder = Decoder::new().context("failed to create H.264 decoder")?;
                    continue;
                }
                flags & (1 << 62) != 0
            }
        };
        let len = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
        if len > 64 * 1024 * 1024 {
            bail!("implausible packet size {len}; stream out of sync");
        }

        packet.resize(len, 0);
        stream.read_exact(&mut packet).context("stream ended mid-packet")?;

        if is_config {
            // SPS/PPS: feed it together with the next frame, like scrcpy does.
            config_packet = packet.clone();
            continue;
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
                    device_name: device_name.clone(),
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

fn find_server(config: &MirrorConfig) -> Result<PathBuf> {
    let candidates = config
        .server_path
        .clone()
        .into_iter()
        .chain(std::env::var_os("SCRCPY_SERVER_PATH").map(PathBuf::from))
        .chain(SERVER_SEARCH_PATHS.iter().map(PathBuf::from));
    for path in candidates {
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!(
        "scrcpy-server not found. Install scrcpy, or set SCRCPY_SERVER_PATH to the scrcpy-server file"
    )
}

/// The server refuses to start unless the client passes its exact version.
fn detect_server_version() -> Result<String> {
    if let Ok(v) = std::env::var("SCRCPY_SERVER_VERSION") {
        return Ok(v);
    }
    let out = std::process::Command::new("scrcpy")
        .arg("--version")
        .output()
        .context("could not run `scrcpy --version` to detect the server version; set SCRCPY_SERVER_VERSION")?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .next()
        .and_then(|line| line.strip_prefix("scrcpy "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .context("could not parse `scrcpy --version`; set SCRCPY_SERVER_VERSION")
}

fn random_scid() -> u32 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    // 31 bits: the server parses scid as a signed int.
    ((nanos as u32) ^ std::process::id().rotate_left(16)) & 0x7fff_ffff
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().context("path is not valid UTF-8")
}
