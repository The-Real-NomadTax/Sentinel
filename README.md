# Sentinel

A desktop GUI for ADB: a device dashboard (model, Android version, chipset, battery, storage, RAM),
screen mirroring you can interact with (in the main window or popped out into its own),
screenshots and screen recording saved on your computer, and APK install. Ctrl+Q quits.

## Using the mirrored screen

- **Left-click / drag**: tap and swipe like a finger
- **Scroll wheel**: scroll
- **Right-click**: Back · **Middle-click**: Home
- **Typing**: click the screen first (it gets a blue outline), then type; Enter, Backspace,
  arrows, Tab, Esc and Ctrl+letter shortcuts (Ctrl+A/C/X…) work on the phone
- **Ctrl+V** pastes your computer's clipboard on the phone. Text the phone's virtual keyboard
  can't type (accented letters, CJK, emoji…) is pasted the same way. Both replace what was on
  the phone's clipboard. Copying on the phone (Ctrl+C) stays on the phone; it isn't sent back.
- Buttons under the screen: Back, Home, Recents, Volume −/+, Power

## Layout

```
agent/             Kotlin on-device agent: screen capture + input injection
crates/
  sentinel-core/   UI-agnostic backend: adb, APK install, mirroring (embeds the agent)
  sentinel-gui/    GPUI frontend (binary: `sentinel`)
```

The frontend only uses `sentinel-core`'s public API (`adb`, `device_info`, `apps`, `media`,
`Mirror` and its `Controller`). To try a different UI toolkit, add a new crate that
depends on `sentinel-core`; nothing in core needs to change.

## Requirements

To run:

- `adb` on `PATH` (override with `SENTINEL_ADB=/path/to/adb`)
- A phone with USB debugging enabled and authorized.

To build, additionally:

- `kotlinc` and a JDK (they run the Kotlin and Android build tools)
- An Android SDK with build-tools and one `platforms/android-*` package
  (`ANDROID_HOME`, `ANDROID_SDK_ROOT` or `~/Android/Sdk`)

## Screenshots and recordings

Both are saved on the computer; nothing is written to the phone.

- **Screenshot** captures the phone at full resolution with Android's built-in `screencap`,
  streamed straight back over adb, into `~/Pictures/Sentinel/`. Works without mirroring.
- **Record** saves the mirroring video stream to an MP4 in `~/Videos/Sentinel/` (it starts
  mirroring if needed). Rotating the phone mid-recording continues in `..._2.mp4`. Recordings are
  at the mirroring resolution (1280px on the long side) and only contain frames when the screen
  changes, so a still screen holds its last frame.

## Exporting installed apps

The **Apps** tab (or **Export installed APKs…** in the sidebar) lists the phone's apps with their
real names, versions and sizes. Type to search; tick **System apps** to include preinstalled ones.
**Export APK** copies the app's APK files to `~/Downloads/Sentinel/` with `adb pull`:

- An app installed as a single APK is saved as `Name_version.apk`.
- An app split into several APKs (most Play Store apps) is saved as a folder `Name_version/` with
  `base.apk` and its `split_*.apk` files. Drop that folder on the window to install it again
  (`adb install-multiple`).

Exports never overwrite: a second export of the same version gets a `(2)` suffix. App names come
from the agent, run once with `mode=apps`; if it can't run, the list falls back to
`pm list packages` and shows package names only.

## How mirroring works

Sentinel ships its own small Kotlin agent (`agent/`, about 30 KB once compiled). `cargo build`
compiles it to Android dex (`agent/build.sh`) and embeds it in the binary. For each mirroring
session Sentinel pushes it to `/data/local/tmp`, starts it with `app_process` as the shell user,
and talks to it over an `adb forward` tunnel:

- **video**: the agent mirrors the display into Android's hardware H.264 encoder and streams it;
  Sentinel decodes it with OpenH264.
- **control**: touches, scrolling, keys and text, injected through Android's input manager.

Each run gets its own jar (`/data/local/tmp/sentinel-agent-<id>.jar`), which the agent deletes
as soon as it starts; it exits when Sentinel disconnects. The tunnel ends in an abstract socket
that any app on the phone could open, so the agent only accepts connections from adbd (the
shell or root user). To build
without the Android toolchain, point `SENTINEL_AGENT_JAR` at a prebuilt `sentinel-agent.jar`.

## Run

```bash
cargo run --release
```

`cargo run --release -- --mirror` starts mirroring automatically once a device is found.

Headless checks for the backend:

```bash
cargo run -p sentinel-core --example probe          # list devices + mirror for 13s
cargo run -p sentinel-core --example install -- app.apk
cargo run -p sentinel-core --example input_probe    # presses Home, Recents, Back on the phone
cargo run -p sentinel-core --example media_probe -- /tmp   # screenshot + 5s recording
```

## Testing the GUI privately

`scripts/virtual-display.sh` runs a command in a headless KWin session, so GUI tests never
touch your real screen:

```bash
scripts/virtual-display.sh -o /tmp/vd-display -- ./target/debug/sentinel --mirror &
DISPLAY=$(cat /tmp/vd-display) xdotool search --name '^Sentinel$'   # drive it with xdotool
import -display "$(cat /tmp/vd-display)" -window <id> shot.png       # capture one window
```

Inside the session the app runs on X11 (XWayland). KWin's own shortcuts like Alt+F4 don't
reach it there, so use Ctrl+Q to quit cleanly.

## Notes

- `Cargo.lock` pins `libc` to 0.2.189: newer versions drop a constant that `xattr 0.2`
  (pulled in by gpui 0.2.2) needs. Don't `cargo update` libc past that until gpui updates.
