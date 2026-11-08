#!/usr/bin/env bash
# Build Sentinel's on-device agent: Kotlin -> JVM bytecode -> Android dex (shrunk by R8),
# packaged as a jar that `app_process` can load.
#
# Usage: agent/build.sh [output.jar]   (default: agent/build/sentinel-agent.jar)
# Needs: kotlinc, java, and an Android SDK (ANDROID_HOME, ANDROID_SDK_ROOT or ~/Android/Sdk)
# with at least one build-tools and one platforms/android-* package.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/build/sentinel-agent.jar}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}
[[ -d $sdk ]] || { echo "Android SDK not found (set ANDROID_HOME)" >&2; exit 1; }
build_tools=$(ls -d "$sdk"/build-tools/* 2>/dev/null | sort -V | tail -1)
platform=$(ls -d "$sdk"/platforms/android-* 2>/dev/null | sort -V | tail -1)
[[ -n $build_tools && -f $build_tools/lib/d8.jar ]] || { echo "no Android build-tools in $sdk" >&2; exit 1; }
[[ -n $platform && -f $platform/android.jar ]] || { echo "no Android platform in $sdk" >&2; exit 1; }
android_jar=$platform/android.jar

command -v kotlinc >/dev/null || { echo "kotlinc not found" >&2; exit 1; }
stdlib=""
for home in "${KOTLIN_HOME:-}" "$(dirname "$(dirname "$(readlink -f "$(command -v kotlinc)")")")" \
    /usr/share/kotlin /usr/lib/kotlin /opt/kotlin; do
    if [[ -n $home && -f $home/lib/kotlin-stdlib.jar ]]; then stdlib=$home/lib/kotlin-stdlib.jar; break; fi
done
[[ -n $stdlib ]] || { echo "kotlin-stdlib.jar not found (set KOTLIN_HOME)" >&2; exit 1; }

kotlinc "$here/src" -no-jdk -cp "$android_jar" -jvm-target 1.8 -nowarn -d "$work/classes.jar"

mkdir -p "$(dirname "$out")"
java -cp "$build_tools/lib/d8.jar" com.android.tools.r8.R8 \
    --release --min-api 26 \
    --lib "$android_jar" \
    --pg-conf "$here/proguard.pro" \
    --output "$work/agent.jar" \
    "$work/classes.jar" "$stdlib"
mv "$work/agent.jar" "$out"
echo "built $out ($(stat -c %s "$out") bytes)"
