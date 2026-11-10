# Camera capture on Android

This guide covers the Android side of the photo feature: how `src/camera.rs`
takes a single still image with NDK Camera2, how the `CAMERA` permission is
requested from Rust, and what to watch out for. General Android and egui setup
is in [`android-egui-rust-setup.md`](android-egui-rust-setup.md).

The photo stays in memory. Nothing is written to storage.

---

## 1. Requirements

| Item | Value in this repo |
| --- | --- |
| `min_sdk_version` | 28 (`Cargo.toml`) |
| `target_sdk_version` | 30 (see pitfall 1) |
| Permission | `android.permission.CAMERA` |
| Native libraries | `camera2ndk` (API 24+), `mediandk` (`AImageReader`, API 26+) |
| Test device | Nothing Phone (Pong), SM8475, Android 16 |

The manifest entry comes from `Cargo.toml`:

```toml
[[package.metadata.android.uses_permission]]
name = "android.permission.CAMERA"
```

Crates used on Android:

- `ndk-sys` 0.6: `AImageReader_*` and `AImage_*` (media calls)
- `ndk-context` 0.1: gives access to the Java VM
- `jni` 0.22: runtime permission check and request
- `image` 0.25 with `jpeg`: decodes the JPEG to RGBA

The camera2 calls (`ACameraManager_*`, `ACameraDevice_*`,
`ACaptureRequest_*`, `ACameraCaptureSession_*`) are declared by hand in
`src/camera.rs`, because `ndk-sys` does not link `camera2ndk`.

---

## 2. How a photo is taken

`take_photo()` runs on a worker thread (the UI calls it from `start_photo`).
It does the following:

1. **Permission.** `permission_granted()` calls `checkSelfPermission` on the
   Activity. If the permission is missing, `request_permission()` calls
   `requestPermissions`, and the function returns a message asking the user to
   tap again. The app does not wait for the dialog.
2. **Find the camera.** `back_camera_id()` lists the camera IDs and returns the
   first one whose `ACAMERA_LENS_FACING` is back.
3. **Pick a size.** `jpeg_size()` reads `ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS`
   and takes the largest JPEG output whose longest side is at most 1920.
4. **Open.** An `AImageReader` (JPEG, 2 buffers) provides the window. The
   camera opens with `ACameraManager_openCamera`.
5. **Capture.** A `TEMPLATE_STILL_CAPTURE` request is made with a single output
   target, and `ACameraCaptureSession_capture` fires it once. JPEG quality is 95.
6. **Read.** `wait_for_jpeg()` polls `AImageReader_acquireNextImage` every
   20 ms for up to 10 seconds. It copies plane 0 (the JPEG bytes), then calls
   `AImage_delete`.
7. **Decode.** The JPEG is decoded to RGBA8 and returned as a `Photo`.

Everything is torn down in reverse order: session, output objects, request,
target, device, reader, and manager.

---

## 3. Pitfalls

### 3.1 `target_sdk_version` must stay at 30

This is a workaround for a `libcdsprpc` loading issue on the Pong, not a camera
requirement. The repo keeps it at 30. Check
[`burn-npu-qnn-setup.md`](burn-npu-qnn-setup.md) before changing it, and test the
NPU and camera paths again after any change.

### 3.2 `libmediandk.so` must be NEEDED

`ndk-sys` provides the `AImageReader_*` symbols, but the calls are reached only
through `ndk-sys` function pointers that the linker can drop. If
`libmediandk.so` is not a NEEDED entry, the app fails at load time with:

```
UnsatisfiedLinkError: ... cannot locate symbol "AImageReader_getWindow"
```

The fix is an empty link block in `src/camera.rs`:

```rust
#[link(name = "mediandk")]
unsafe extern "C" {}
```

Check with:

```sh
llvm-readelf -d lib/arm64-v8a/libburn_phone.so | grep mediandk
```

### 3.3 `ndk_context` holds the Application, not the Activity

`android-activity` initializes `ndk_context` with the **Application** object.
`checkSelfPermission` exists on `Context`, so it works, but `requestPermissions`
exists only on `Activity`. Calling it on the `ndk_context` object fails with
"Method not found: requestPermissions ([Ljava/lang/String;I)V".

The code gets the Activity pointer from `AndroidApp::activity_as_ptr()` and
stores it with `camera::set_activity` at startup (in `android_main`). Do not
use `ndk_context::android_context().context()` for the Activity.

### 3.4 Permission is asynchronous

`requestPermissions` returns immediately. The user answers in a system dialog,
and the result arrives in `onRequestPermissionsResult`, which this app does not
handle. The first tap returns a "tap again" message, and the second tap checks
the permission again. A denied permission gives the same message until the user
grants it in system settings.

### 3.5 Status codes

Every camera2 and media call returns a status. `0` means success. The image
loop treats `AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE` (`-30001`) as "not ready yet"
and keeps polling. Any other error is returned with the operation name.

### 3.6 Only pick sizes the camera advertises

`jpeg_size()` takes sizes only from `ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS`
for JPEG output. Hard-coding a size is a risk, because the session may not
accept it.

### 3.7 Logging

Logs come through `android_logger` with the tag `burn-phone`. Use
`adb logcat -s burn-phone:V AndroidRuntime:E`. Camera errors also appear on
screen, because the UI shows the error string.

### 3.8 Emulators (not tested)

The emulator has not been tested for the camera path. Expect it to report
"no back-facing camera found" or "no JPEG output size" on screen if it has no
suitable camera. Use a real device for the capture test.

---

## 4. Links

- NDK Camera2 reference: <https://developer.android.com/ndk/reference/group/camera>
- NDK media (AImageReader): <https://developer.android.com/ndk/reference/group/media>
- Runtime permissions: <https://developer.android.com/training/permissions/requesting>
- Camera2 overview: <https://developer.android.com/media/camera/camera2>
- Makepad camera reference (commit 506ec05): `platform/hal/src/backend/android/media/camera.rs`
- `ndk-sys`: <https://crates.io/crates/ndk-sys>
- `jni`: <https://crates.io/crates/jni>
- `android-activity` (source of the `ndk_context` behavior): <https://crates.io/crates/android-activity>
