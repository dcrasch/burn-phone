# ---------------------------------------------------------------------------
# burn-phone — Android dev helpers
#
# Override any of these with environment variables, e.g.:
#   AVD=Pixel_8 ANDROID_SERIAL=emulator-5554 just run
# ---------------------------------------------------------------------------

sdk := env_var_or_default("ANDROID_HOME", home_directory() / "Android" / "Sdk")
ndk_version := env_var_or_default("NDK_VERSION", "30.0.16248370")
##ndk_version := "28.2.13676358"
ndk := env_var_or_default("ANDROID_NDK_HOME", sdk / "ndk" / ndk_version)
avd := env_var_or_default("AVD", "Medium_Phone")
emulator_flags := env_var_or_default("EMULATOR_FLAGS", "")
serial := env_var_or_default("ANDROID_SERIAL", "")
java_home := env_var_or_default("JAVA_HOME", home_directory() / "android-studio" / "jbr")
release_keystore := env_var_or_default("CARGO_APK_RELEASE_KEYSTORE", home_directory() / ".android" / "debug.keystore")
release_keystore_password := env_var_or_default("CARGO_APK_RELEASE_KEYSTORE_PASSWORD", "android")

# Qualcomm AI Runtime (QAIRT / QNN) SDK used for build-time bindings and the
# Android arm64 runtime libraries. `just qnn-setup` downloads it on demand.
qnn_version := env_var_or_default("QNN_VERSION", "2.52.0.261104")
qnn_cache := env_var_or_default("QNN_CACHE_DIR", home_directory() / ".cache" / "burn-phone")
qnn_root := env_var_or_default("QNN_SDK_ROOT", qnn_cache / "qairt" / qnn_version)
# Hexagon architectures to package. V79 covers Snapdragon 8 Gen 3 / 8 Elite,
# V73/V75 cover 8 Gen 2 and 7-series, V69/V68 older flagships.
qnn_hexagon := env_var_or_default("QNN_HEXAGON_VERSIONS", "68 69 73 75 79 81")

export ANDROID_HOME := sdk
export ANDROID_NDK_HOME := ndk
export JAVA_HOME := java_home
export QNN_SDK_ROOT := qnn_root
export PATH := sdk / "platform-tools" + ":" + sdk / "emulator" + ":" + java_home / "bin" + ":" + env_var("PATH")
# Local release builds are signed with the debug keystore by default so they
# install without extra setup. Set these env vars to use a real release key.
export CARGO_APK_RELEASE_KEYSTORE := release_keystore
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD := release_keystore_password

# Show available recipes.
default:
    @just --list

# Check the Android/Rust toolchain this justfile will use.
doctor:
    #!/usr/bin/env bash
    set -u
    echo "export ANDROID_HOME=$ANDROID_HOME"
    echo "export ANDROID_NDK_HOME=$ANDROID_NDK_HOME"
    echo "export JAVA_HOME=$JAVA_HOME"
    echo "export AVD={{avd}}"
    echo "ANDROID_SERIAL={{ if serial == "" { "(auto)" } else { serial } }}"
    echo
    for path in "$ANDROID_HOME" "$ANDROID_NDK_HOME" "$JAVA_HOME"; do
        [ -d "$path" ] && echo "ok      $path" || echo "MISSING $path"
    done
    for bin in adb emulator cargo-apk java; do
        if command -v "$bin" >/dev/null 2>&1; then
            echo "ok      $bin ($(command -v "$bin"))"
        else
            echo "MISSING $bin"
        fi
    done
    echo
    echo "QNN_SDK_ROOT     = $QNN_SDK_ROOT"
    if [ -f "$QNN_SDK_ROOT/include/QNN/QnnInterface.h" ]; then
        echo "ok      QNN SDK headers"
    else
        echo "MISSING QNN SDK (run: just qnn-setup)"
    fi
    for lib in libQnnHtp.so libQnnHtpPrepare.so libQnnSystem.so; do
        if [ -f "qnn-libs/arm64-v8a/$lib" ]; then
            echo "ok      packaged $lib"
        else
            echo "MISSING packaged $lib (run: just qnn-libs)"
        fi
    done
    echo
    echo "Installed Rust Android targets:"
    rustup target list --installed | grep -E 'android|^$' || true
    echo
    echo "Connected devices:"
    adb devices -l || true

# One-time setup: install Rust Android targets and cargo-apk.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    rustup target add aarch64-linux-android x86_64-linux-android
    if command -v cargo-apk >/dev/null 2>&1; then
        echo "cargo-apk already installed: $(cargo apk version)"
    else
        cargo install cargo-apk --locked
    fi

android-setup:
    $ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager  "ndk;30.0.16248370"
    $ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager  "platforms;android-30"

# Download and extract the Qualcomm AI Runtime (QNN) SDK.
# Defaults to ~/.cache/burn-phone/qairt/<version>; override with QNN_SDK_ROOT.
qnn-setup:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{qnn_root}}"
    version="{{qnn_version}}"
    cache="{{qnn_cache}}"

    if [ -f "$root/include/QNN/QnnInterface.h" ]; then
        echo "QNN SDK already available: $root"
        exit 0
    fi

    mkdir -p "$cache/downloads" "$cache/extract"
    zip="$cache/downloads/v$version.zip"
    if [ ! -f "$zip" ]; then
        url="https://softwarecenter.qualcomm.com/api/download/software/sdks/Qualcomm_AI_Runtime_Community/All/$version/v$version.zip"
        echo "Downloading $url"
        curl -fL --retry 3 -o "$zip.part" "$url"
        mv "$zip.part" "$zip"
    fi

    rm -rf "$cache/extract"
    mkdir -p "$cache/extract"
    unzip -q "$zip" -d "$cache/extract"

    src="$cache/extract/qairt/$version"
    if [ ! -d "$src" ]; then
        src="$(find "$cache/extract" -type d -path "*/qairt/$version" | head -n1)"
    fi
    if [ -z "$src" ] || [ ! -d "$src" ]; then
        echo "Could not find qairt/$version in the extracted archive" >&2
        exit 1
    fi

    mkdir -p "$(dirname "$root")"
    rm -rf "$root"
    cp -a "$src/." "$root/"
    echo "QNN SDK ready: $root"

# Copy the QNN arm64 Android runtime libraries into qnn-libs/ for the APK.
qnn-libs: qnn-setup
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{qnn_root}}"
    abi="qnn-libs/arm64-v8a"
    mkdir -p "$abi" qnn-libs/x86_64
    rm -f "$abi"/*.so

    for lib in libQnnHtp.so libQnnHtpPrepare.so libQnnSystem.so; do
        cp -f "$root/lib/aarch64-android/$lib" "$abi/"
    done

    for v in {{qnn_hexagon}}; do
        stub="$root/lib/aarch64-android/libQnnHtpV${v}Stub.so"
        skel="$root/lib/hexagon-v${v}/unsigned/libQnnHtpV${v}Skel.so"
        if [ -f "$stub" ]; then
            cp -f "$stub" "$abi/"
        else
            echo "skip V$v stub (not in this SDK)"
        fi
        if [ -f "$skel" ]; then
            cp -f "$skel" "$abi/"
        else
            echo "skip V$v skel (not in this SDK)"
        fi
    done

    du -sh "$abi"
    ls -1 "$abi"

# Show the QNN runtime libraries that will be packaged.
qnn-status:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "QNN_SDK_ROOT = $QNN_SDK_ROOT"
    if [ -f "$QNN_SDK_ROOT/include/QNN/QnnInterface.h" ]; then
        grep -E "QNN_API_VERSION_(MAJOR|MINOR|PATCH)" "$QNN_SDK_ROOT/include/QNN/QnnCommon.h"
    else
        echo "QNN SDK not found (run: just qnn-setup)"
    fi
    echo
    echo "Packaged libraries:"
    ls -lh qnn-libs/arm64-v8a/*.so 2>/dev/null || echo "  (none; run: just qnn-libs)"

# Build a debug APK for every configured Android target.
build:
    cargo apk build

# Build the APK with the fused Hexagon NPU graph (burn-npu `support-hexagon`).
build-fused: build

# Build and run the fused build on the selected device.
run-fused:
    cargo apk run {{ if serial != "" { "--device " + serial } else { "" } }}

# Build a release APK for every configured Android target.
build-release: debug-keystore
    cargo apk build --release

# Make sure the debug keystore exists (used for local release signing).
debug-keystore:
    #!/usr/bin/env bash
    set -euo pipefail
    store="$HOME/.android/debug.keystore"
    if [ ! -f "$store" ]; then
        mkdir -p "$(dirname "$store")"
        keytool -genkeypair -v -keystore "$store" -storepass android -keypass android \
            -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
            -dname "CN=Android Debug,O=Android,C=US"
    fi

# Run on the single connected device / emulator (builds, installs, streams logcat).
run:
    cargo apk run {{ if serial != "" { "--device " + serial } else { "" } }}

# Like `run`, but return immediately instead of streaming logcat.
run-no-logcat:
    cargo apk run --no-logcat {{ if serial != "" { "--device " + serial } else { "" } }}

# Build and run a release build on the selected device.
run-release: debug-keystore
    cargo apk run --release {{ if serial != "" { "--device " + serial } else { "" } }}

# Deploy to a real phone connected over USB (ignores emulators).
run-phone:
    #!/usr/bin/env bash
    set -euo pipefail
    target="{{ serial }}"
    if [ -z "$target" ]; then
        target=$(adb devices | awk 'NR > 1 && $2 == "device" && $1 !~ /^emulator-/ { print $1; exit }')
    fi
    if [ -z "$target" ]; then
        echo "No physical phone found." >&2
        echo "Enable Developer Options + USB debugging, authorize the computer, then run: just devices" >&2
        exit 1
    fi
    echo "Deploying to phone: $target"
    cargo apk run --device "$target"

# Deploy to the running emulator.
run-emulator:
    #!/usr/bin/env bash
    set -euo pipefail
    target=$(adb devices | awk 'NR > 1 && $2 == "device" && $1 ~ /^emulator-/ { print $1; exit }')
    if [ -z "$target" ]; then
        echo "No emulator is running. Start one with: just emulator" >&2
        exit 1
    fi
    echo "Deploying to emulator: $target"
    cargo apk run --device "$target"

# Start the AVD in the background and wait until Android has booted.
emulator:
    #!/usr/bin/env bash
    set -euo pipefail
    log="${TMPDIR:-/tmp}/burn-phone-emulator-{{ avd }}.log"
    pidfile="${TMPDIR:-/tmp}/burn-phone-emulator-{{ avd }}.pid"

    if [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
        echo "Emulator already running (pid $(cat "$pidfile"))."
    else
        echo "Starting AVD '{{ avd }}' (log: $log)"
        nohup emulator -avd "{{ avd }}" -netdelay none -netspeed full -no-boot-anim {{ emulator_flags }} \
            >"$log" 2>&1 &
        echo $! >"$pidfile"
    fi

    echo "Waiting for the emulator to appear..."
    adb -e wait-for-device
    echo "Waiting for Android to finish booting..."
    until [ "$(adb -e shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; do
        sleep 1
    done
    adb -e shell input keyevent 82 >/dev/null 2>&1 || true
    echo
    echo "Emulator ready:"
    adb devices -l

# Run the emulator in the foreground (Ctrl-C to stop).
emulator-fg:
    emulator -avd "{{ avd }}" -netdelay none -netspeed full {{ emulator_flags }}

# Shut the running emulator down.
emulator-stop:
    -adb -e emu kill

# List connected devices.
devices:
    adb devices -l

# Follow the app's logcat output.
logs:
    adb logcat -v color -s burn-phone:V AndroidRuntime:E

# Follow all device logs.
logcat:
    adb logcat -v color

# Format the code.
fmt:
    cargo fmt --all

# Remove build artifacts.
clean:
    cargo clean
