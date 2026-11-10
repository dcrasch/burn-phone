# Setting up an Android egui/Rust app

This guide describes the toolchain and project layout used by this repository:
a Rust `cdylib` built with [`cargo-apk`](https://github.com/rust-mobile/cargo-apk),
running an [`eframe`/`egui`](https://github.com/emilk/egui) UI through
Android's `NativeActivity`.

The result is a normal APK that can be installed on a phone or emulator.

Qualcomm NPU support (the QNN runtime, `burn-npu`, and the Hexagon libraries) is
covered separately in [`burn-npu-qnn-setup.md`](burn-npu-qnn-setup.md). This
guide covers only the general Android and egui setup.

---

## 1. Host requirements

Tested on Linux (Ubuntu 24.04) with:

| Tool | Version used here |
| --- | --- |
| Rust | 1.92 or newer |
| Android SDK platform | API 35 |
| Android build-tools | 36.0.0 |
| Android NDK | 30.0.16248370 |
| JDK | 17+ (Android Studio's bundled JBR works) |
| `cargo-apk` | 0.10 |
| `just` | optional, but the commands below use it |

For x86_64 emulators, KVM must be available:

```sh
sudo apt install qemu-kvm
sudo usermod -aG kvm "$USER"   # log out and back in
ls -l /dev/kvm
```

Without `/dev/kvm` the emulator falls back to software emulation and is
painfully slow.

---

## 2. Install the Android SDK

You can install the SDK through Android Studio or with the command-line tools.
The command-line route is reproducible and sufficient for this project.

### 2.1 Command-line tools

Download "Command line tools only" from
<https://developer.android.com/studio#command-tools>, then:

```sh
export ANDROID_HOME="$HOME/Android/Sdk"
mkdir -p "$ANDROID_HOME/cmdline-tools"
unzip commandline-tools-linux.zip -d "$ANDROID_HOME/cmdline-tools"
mv "$ANDROID_HOME/cmdline-tools/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
```

### 2.2 SDK packages

```sh
export ANDROID_HOME="$HOME/Android/Sdk"
export PATH="$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH"

sdkmanager --licenses
sdkmanager \
  "platform-tools" \
  "platforms;android-35" \
  "build-tools;36.0.0" \
  "ndk;30.0.16248370" \
  "emulator" \
  "system-images;android-35;google_apis;x86_64"
```

On an ARM64 host, replace the emulator system image with
`system-images;android-35;google_apis;arm64-v8a`.

If you use Android Studio, the same packages are available through
**Settings → Languages & Frameworks → Android SDK** and **SDK Tools**.

### 2.3 Environment variables

Add these to `~/.bashrc` (or your shell's equivalent):

```sh
export ANDROID_HOME="$HOME/Android/Sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/30.0.16248370"
export JAVA_HOME="$HOME/android-studio/jbr"   # or your JDK 17+ install
export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$ANDROID_HOME/cmdline-tools/latest/bin:$JAVA_HOME/bin:$PATH"
```

Verify:

```sh
adb version
sdkmanager --version
java -version
grep Pkg.Revision "$ANDROID_NDK_HOME/source.properties"
```

### 2.4 Create an emulator

The AVD name used by this repository is `Medium_Phone`:

```sh
avdmanager create avd \
  --name Medium_Phone \
  --package "system-images;android-35;google_apis;x86_64" \
  --device "pixel_6"
```

Or create one interactively in Android Studio's **Device Manager**.

Start it headless or with a window:

```sh
emulator -avd Medium_Phone -netdelay none -netspeed full
# or
emulator -avd Medium_Phone -no-window -no-audio
```

---

## 3. Install the Rust Android toolchain

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-apk --locked
```

`aarch64-linux-android` is for phones, `x86_64-linux-android` is for the
standard emulator. Build both so the same APK works in either place.

`cargo-apk` is a separate binary from the deprecated `cargo apk` of the
`android-rs-glue` era. Confirm with:

```sh
cargo apk version
# cargo-apk 0.10.x
```

---

## 4. Project layout

A `cargo apk` package must produce a shared library, not a binary. A binary
target in the same package breaks `cargo apk build`, so the app lives entirely
in the library:

```text
burn-phone/
├── Cargo.toml            # Android library package
├── src/lib.rs            # egui app, android_main()
├── justfile
└── qnn-libs/             # optional runtime .so files (see burn-npu guide)
```

`Cargo.toml`:

```toml
[package]
name = "burn-phone"
version = "0.1.0"
edition = "2024"
rust-version = "1.92"

[lib]
name = "burn_phone"
crate-type = ["cdylib", "rlib"]

[dependencies]
eframe = { version = "0.37", default-features = false, features = [
    "default_fonts",
    "glow",
] }
log = "0.4"

[target.'cfg(target_os = "android")'.dependencies]
winit = { version = "0.30", default-features = false, features = ["android-native-activity"] }
android_logger = "0.15"

[profile.dev.package."*"]
opt-level = 2
```

---

## 5. Android packaging metadata

`cargo-apk` reads `[package.metadata.android]` from the package manifest:

```toml
[package.metadata.android]
package = "dev.burnphone.app"
apk_name = "burn-phone"
build_targets = ["aarch64-linux-android", "x86_64-linux-android"]
strip = "strip"

# Optional: extra .so files to package. The directory must contain one
# subdirectory per ABI (arm64-v8a, x86_64, ...).
runtime_libs = "qnn-libs"

[package.metadata.android.sdk]
min_sdk_version = 28
target_sdk_version = 30

[package.metadata.android.application]
label = "Burn Phone"
theme = "@android:style/Theme.DeviceDefault.NoActionBar.Fullscreen"

[package.metadata.android.application.activity]
config_changes = "orientation|keyboardHidden|screenSize|screenLayout|smallestScreenSize|uiMode|density"
orientation = "unspecified"
```

Notes:

- `min_sdk_version = 28` is required by the bundled QNN runtime; a plain egui
  app can use a lower value.
- `target_sdk_version = 30` is set for the NPU runtime. Apps targeting Android
  12 (API 31) or newer cannot load the vendor `libcdsprpc.so` unless the
  manifest declares it with `<uses-native-library>`, and cargo-apk 0.10 cannot
  emit that element. The reasoning and the `libcdsprpc.so` error are in
  [QNN guide, section 5.3](burn-npu-qnn-setup.md#53-manifest-configuration).
  A plain egui app without QNN can raise this value.
- `strip = "strip"` runs `llvm-objcopy --strip-debug` on every packaged `.so`.
  Remove it if you need native debug symbols.
- If `runtime_libs` is set, every ABI in `build_targets` must have a
  corresponding directory, even an empty one. Otherwise `cargo-apk` fails
  opening a missing directory. Use `.gitkeep` files for empty ABI folders.
- The generated APKs are:
  - `target/debug/apk/burn-phone.apk`
  - `target/release/apk/burn-phone.apk`

---

## 6. Entry points

The library exports the Android `android_main` entry point.

```rust
pub use eframe;
use eframe::egui;

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 760.0])
            .with_min_inner_size([320.0, 480.0])
            .with_title("Burn Phone"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

fn run_app(options: eframe::NativeOptions) -> eframe::Result {
    eframe::run_native(
        "Burn Phone",
        options,
        Box::new(|cc| Ok(Box::new(MyApp::new(cc)))),
    )
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("burn-phone"),
    );

    let mut options = native_options();
    options.android_app = Some(app);

    if let Err(err) = run_app(options) {
        log::error!("eframe failed to start: {err}");
    }
}
```

Important details:

- `#[unsafe(no_mangle)]` is required in edition 2024 (previously
  `#[no_mangle]`).
- `android_main` must be `pub` and have exactly this name; `android-activity`
  looks it up dynamically.
- The `android-native-activity` feature on `winit` enables the NativeActivity
  glue.
- `eframe` is configured with the `glow` (OpenGL) renderer. Some older devices
  do not support the default wgpu path; glow is the most compatible choice.
- `android_logger` sends `log` output to logcat under the `burn-phone` tag.
- The repository's `android_main` also sets `ADSP_LIBRARY_PATH` and preloads
  `libcdsprpc.so` before the UI starts. Plain egui apps do not need this. See
  [QNN guide, section 6](burn-npu-qnn-setup.md#6-runtime-setup-on-android).

---

## 7. Build, install, run

The repository's `justfile` wraps the common commands:

```sh
just setup          # rustup targets + cargo-apk
just doctor         # check SDK/NDK/JDK/adb/emulator paths
just emulator       # boot the AVD in the background
just run            # build, install, launch, stream logcat
just run-no-logcat  # build, install, launch, return immediately
just logs           # follow only this app's logcat
just run-phone      # deploy to a physical phone (skips emulators)
just build          # debug APK only
just build-fused    # debug APK with the fused Hexagon NPU graph
just build-release  # release APK, signed with the debug key by default
```

The equivalent raw commands are:

```sh
cargo apk build
cargo apk run --device emulator-5554
adb install -r target/debug/apk/burn-phone.apk
adb shell am start -n dev.burnphone.app/android.app.NativeActivity
```

### Release signing

`cargo-apk` uses the Android debug keystore by default for local release
builds. To use a real key:

```sh
export CARGO_APK_RELEASE_KEYSTORE=/path/to/release.keystore
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=secret
cargo apk build --release
```

The keystore alias is expected to be `androiddebugkey` unless the environment
variables are adjusted.

---

## 8. Classifier and benchmark

The app captures a still photo, downsizes it so the longest side is at most
640 px (`MAX_SIDE` in `src/camera.rs`), and keeps it in memory only. Each photo
is classified automatically by SqueezeNet 1.1 on `NpuBurnBackend`, and the UI
shows the top 4 classes with percentages next to the photo.

The benchmark button runs the same image through the CPU and NPU paths and
shows both times. The model and its ops are described in
[`burn-npu-qnn-setup.md`](burn-npu-qnn-setup.md) sections 7.1–7.4.

The same comparison is available from the command line with `just benchmark`
on a connected phone; see [`SKILL.md`](SKILL.md) for the workflow.

---

## 9. Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `error: failed to run custom build command for 'ndk-sys'` | `ANDROID_NDK_HOME` points at a missing or wrong NDK version. |
| `bindgen failed` with `'gnu/stubs-32.h' file not found` during an Android build | Bindgen is using host glibc headers instead of the NDK sysroot. Set `ANDROID_NDK_HOME`; the QNN guide's [section 4.1](burn-npu-qnn-setup.md#41-android-cross-builds) covers the fix. |
| `library "libcdsprpc.so" ... is not accessible for the namespace` | The app targets API 31+ and tries to use a vendor library. See the `target_sdk_version` note in section 5 and the QNN guide's [section 5.3](burn-npu-qnn-setup.md#53-manifest-configuration). |
| `error: linking with 'cc' failed` while building `aarch64-linux-android` | Rust Android target not installed (`rustup target add aarch64-linux-android`). |
| `cargo apk` says "no library target found" | The package has no `[lib] crate-type = ["cdylib"]`, or a `src/main.rs` bin target shadows it. Remove the binary target and keep the app in the library. |
| APK installs but the app closes immediately | Check `adb logcat -s burn-phone:V AndroidRuntime:E`; usually a missing `android_main`, a panic in `run_native`, or an unsupported GL driver. |
| `UnsatisfiedLinkError: dlopen failed: library "libc++_shared.so" not found` | A native dependency uses the shared C++ runtime; package it from `$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/<abi>/libc++_shared.so`. |
| `adb devices` shows `unauthorized` | Accept the USB debugging prompt on the phone; revoke/re-authorize USB debugging if needed. |
| Emulator is extremely slow | `/dev/kvm` missing or the user is not in the `kvm` group. |
| `INSTALL_FAILED_NO_MATCHING_ABIS` | The APK has no library for the device ABI; build `aarch64-linux-android` for phones or `x86_64-linux-android` for the emulator. |
| Native library fails to load on Android 15+ | Verify 16 KB page alignment with `llvm-readelf -l libfoo.so | grep LOAD`; rebuild libraries with NDK r28+ if necessary. |
| `cargo apk build` fails inside `aapt`/`zipalign` | Build-tools version mismatch; install the version cargo-apk expects and make sure `ANDROID_HOME` is correct. |

Useful ADB commands:

```sh
adb devices -l
adb shell getprop ro.product.cpu.abi
adb shell getprop ro.build.version.sdk
adb install -r target/debug/apk/burn-phone.apk
adb shell am force-stop dev.burnphone.app
adb logcat -c && adb logcat -s burn-phone:V
```

---

## References

**Android SDK, NDK, and tools**

- [Android Studio and command-line tools](https://developer.android.com/studio#command-tools)
- [`sdkmanager`](https://developer.android.com/tools/sdkmanager) (deprecated in favor of `android sdk`)
- [`avdmanager`](https://developer.android.com/tools/avdmanager) (deprecated in favor of `android emulator`)
- [Android Debug Bridge (`adb`)](https://developer.android.com/tools/adb)
- [NDK downloads](https://developer.android.com/ndk/downloads) and [NDK revision history](https://developer.android.com/ndk/downloads/revision_history)
- [Emulator hardware acceleration](https://developer.android.com/studio/run/emulator-acceleration)
- [`NativeActivity` reference](https://developer.android.com/ndk/reference/group/native-activity)
- [`<uses-native-library>` manifest element](https://developer.android.com/guide/topics/manifest/uses-native-library-element)
- [Android 12 behavior changes](https://developer.android.com/about/versions/12/behavior-changes-12)

**Rust and build tooling**

- [rustup cross-compilation](https://rust-lang.github.io/rustup/cross-compilation.html)
- [`cargo-apk`](https://github.com/rust-mobile/cargo-apk)
- [`just`](https://just.systems)

**UI and windowing**

- [`egui`](https://github.com/emilk/egui) and [`eframe` on docs.rs](https://docs.rs/eframe)
- [`winit`](https://github.com/rust-windowing/winit)
- [`android-activity`](https://github.com/rust-mobile/android-activity)

**Related in this repository**

- [`burn-npu-qnn-setup.md`](burn-npu-qnn-setup.md): QNN SDK, burn-npu, and Hexagon NPU setup
- [`burn-npu`](https://github.com/frane/burn-npu) upstream repository
- [Burn](https://burn.dev) deep learning framework
- [Qualcomm AI Engine Direct SDK](https://www.qualcomm.com/developer/software/qualcomm-ai-engine-direct-sdk)
