# burn-phone

This project uses burn 0.21 , maybe later upgrade to 0.22 because of breaking changes.

A small [egui](https://github.com/emilk/egui) / `eframe` Android app built with
[`cargo-apk`](https://github.com/rust-mobile/cargo-apk) (`android-activity` +
`NativeActivity`).

## Documentation

- [Setting up an Android egui/Rust app](docs/android-egui-rust-setup.md) —
  Android SDK/NDK, Rust targets, `cargo-apk`, project layout, and deployment.
- [Setting up burn-npu with the Qualcomm QNN SDK](docs/burn-npu-qnn-setup.md) —
  QNN SDK download, bindgen, Hexagon runtime libraries, and on-device
  verification.
- [QNN graph-build pitfalls](docs/qnn-graph-build-pitfalls.md) — duplicate
  graph/name ids and tensor parameter registration failures on the Hexagon DSP.
- [Hexagon benchmark skill](.opencode/skills/hexagon-benchmark/SKILL.md) —
  `just benchmark` workflow for measuring CPU vs NPU on a physical phone.

## Quick start

```sh
just setup      # install Rust Android targets + cargo-apk (once)
just doctor     # verify SDK, NDK, JDK, adb, emulator
just emulator   # boot the AVD in the background
just run        # build, install, launch, follow logcat
```

On a real phone: enable Developer Options and USB debugging, plug it in,
authorize the computer, then:

```sh
just devices
just run-phone
```

Useful recipes:

| Recipe | What it does |
| --- | --- |
| `just build` / `just build-release` | Build debug/release APKs |
| `just build-fused` / `just run-fused` | Build/run the fused Hexagon NPU graph |
| `just run` | Run on the only connected device, streaming logcat |
| `just run-phone` | Run on a physical phone (skips emulators) |
| `just run-emulator` | Run on the running emulator |
| `just emulator` | Start the AVD and wait for boot |
| `just emulator-fg` | Run the emulator in the foreground |
| `just emulator-stop` | Shut the emulator down |
| `just logs` | Follow only this app's logcat output |
| `just devices` | Show connected devices |

## Configuration

The justfile reads these environment variables (override as needed):

| Variable | Default |
| --- | --- |
| `ANDROID_HOME` | `~/Android/Sdk` |
| `ANDROID_NDK_HOME` | `$ANDROID_HOME/ndk/30.0.16248370` |
| `NDK_VERSION` | `30.0.16248370` |
| `JAVA_HOME` | `~/android-studio/jbr` |
| `AVD` | `Medium_Phone` |
| `EMULATOR_FLAGS` | *(empty, e.g. `-no-window` for headless)* |
| `ANDROID_SERIAL` | *(auto-detect)* |

`cargo-apk` builds the targets listed in `Cargo.toml`
(`aarch64-linux-android` and `x86_64-linux-android`), so the same APK works on
phones and on x86_64 emulators.

Release builds are signed with the Android debug keystore by default so they
install locally. For a distributable build, point cargo-apk at a real key:

```sh
CARGO_APK_RELEASE_KEYSTORE=/path/to/release.keystore \
CARGO_APK_RELEASE_KEYSTORE_PASSWORD=secret \
just build-release
```

## Layout

- `src/lib.rs` — the egui app plus `android_main` (Android entry point)
- `Cargo.toml` — `cdylib` target and `[package.metadata.android]` APK config
- `justfile` — build / emulator / deploy helpers
