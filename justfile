# Sentinel task runner. Run `just` to list recipes.

set positional-arguments

# List available recipes
default:
    @just --list

# Run the app (release build: video decoding is slow in debug)
run *args:
    cargo run --release -- "$@"

# Run the app and start mirroring as soon as a device is found
mirror:
    cargo run --release -- --mirror

# Run a debug build of the app
dev *args:
    cargo run -- "$@"

# Build an optimized binary at target/release/sentinel
build:
    cargo build --release

# Lint and test the whole workspace
check:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Format all crates
fmt:
    cargo fmt --all

# Headless backend check: list devices and mirror for a few seconds
probe:
    cargo run --release -p sentinel-core --example probe

# Install an APK on the first ready device without the GUI
install apk:
    cargo run --release -p sentinel-core --example install -- "$1"

# Show connected devices
devices:
    adb devices -l

# Remove leftover adb port forwards from killed mirroring sessions
clean-forwards:
    adb forward --list | awk '$3 ~ /^localabstract:sentinel_/ {print $1, $2}' | while read -r serial port; do adb -s "$serial" forward --remove "$port"; done

# Build just the on-device Kotlin agent (to agent/build/sentinel-agent.jar)
agent:
    agent/build.sh

# Remove build artifacts
clean:
    cargo clean
