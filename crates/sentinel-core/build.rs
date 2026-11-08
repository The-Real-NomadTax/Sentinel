//! Builds the Kotlin on-device agent (`agent/` at the repo root) and embeds it in the binary.
//!
//! Needs `kotlinc`, `java` and an Android SDK; see `agent/build.sh`. To skip the Kotlin build
//! (e.g. on a machine without the SDK), point `SENTINEL_AGENT_JAR` at a prebuilt agent jar.

use std::{env, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("sentinel-agent.jar");
    println!("cargo:rerun-if-env-changed=SENTINEL_AGENT_JAR");

    if let Ok(prebuilt) = env::var("SENTINEL_AGENT_JAR") {
        println!("cargo:rerun-if-changed={prebuilt}");
        std::fs::copy(&prebuilt, &out).unwrap_or_else(|e| panic!("copying SENTINEL_AGENT_JAR={prebuilt}: {e}"));
        return;
    }

    let agent_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../agent");
    for path in ["src", "build.sh", "proguard.pro"] {
        println!("cargo:rerun-if-changed={}", agent_dir.join(path).display());
    }

    let status = Command::new("bash")
        .arg(agent_dir.join("build.sh"))
        .arg(&out)
        .status()
        .expect("failed to run agent/build.sh");
    if !status.success() {
        panic!(
            "building the Kotlin agent failed (see output above). It needs kotlinc, java and an \
             Android SDK (ANDROID_HOME or ~/Android/Sdk); or set SENTINEL_AGENT_JAR to a prebuilt jar."
        );
    }
}
