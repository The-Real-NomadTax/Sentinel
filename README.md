# Sentinel

A desktop GUI for ADB: mirror your Android phone's screen and install APKs.

## Layout

```
crates/
  sentinel-core/   UI-agnostic backend: adb devices, APK install, screen mirroring
  sentinel-gui/    GPUI frontend (binary: `sentinel`)
```

The frontend only uses `sentinel-core`'s public API (`adb::list_devices`, `adb::install_apk`,
`Mirror::start` / `take_frame` / `status`). To try a different UI toolkit, add a new crate that
depends on `sentinel-core`; nothing in core needs to change.

## Requirements

- `adb` on `PATH` (override with `SENTINEL_ADB=/path/to/adb`)
- `scrcpy` installed. Sentinel reuses its small on-device server (`scrcpy-server`) for
  low-latency video, and decodes the H.264 stream itself with OpenH264.
  Override with `SCRCPY_SERVER_PATH` / `SCRCPY_SERVER_VERSION` if it's in an unusual place.
- A phone with USB debugging enabled and authorized.

## Run

```bash
cargo run --release
```

`cargo run --release -- --mirror` starts mirroring automatically once a device is found.

Headless checks for the backend:

```bash
cargo run -p sentinel-core --example probe          # list devices + mirror for 13s
cargo run -p sentinel-core --example install -- app.apk
```

## Notes

- `Cargo.lock` pins `libc` to 0.2.189: newer versions drop a constant that `xattr 0.2`
  (pulled in by gpui 0.2.2) needs. Don't `cargo update` libc past that until gpui updates.
