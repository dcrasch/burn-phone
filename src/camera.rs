//! Single still capture from the device camera.
//!
//! Uses NDK Camera2 and needs the `CAMERA` permission, which is requested
//! through JNI from the UI.

/// A captured still image, decoded to tightly packed RGBA8.
pub struct Photo {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Captures one still image from the default camera.
pub fn take_photo() -> Result<Photo, String> {
    imp::take_photo()
}

#[cfg(not(target_os = "android"))]
mod imp {
    use super::Photo;

    pub fn take_photo() -> Result<Photo, String> {
        Err("camera capture is only supported on Android".to_string())
    }
}

/// Records the Android `Activity` pointer, needed for the permission request.
#[cfg(target_os = "android")]
pub fn set_activity(ptr: *mut std::ffi::c_void) {
    imp::set_activity(ptr);
}

#[cfg(target_os = "android")]
mod imp {
    use super::Photo;
    use std::ffi::{CStr, CString, c_void};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use jni::objects::{JObject, JValue};

    const PERMISSION: &str = "android.permission.CAMERA";
    const LENS_FACING: u32 = 524293;
    const LENS_FACING_BACK: u8 = 1;
    const STREAM_CONFIGS: u32 = 851978;
    const STREAM_OUTPUT: i32 = 0;
    const JPEG_QUALITY: u32 = 458756;
    const TEMPLATE_STILL_CAPTURE: u32 = 2;
    const FORMAT_JPEG: i32 = 0x100;
    const MAX_SIDE: i32 = 640;
    const JPEG_QUALITY_VALUE: u8 = 95;
    const WAIT: Duration = Duration::from_secs(10);
    const NO_IMAGE: i32 = -30001;

    // Keeps libmediandk in NEEDED even though the image calls go through ndk-sys;
    // without it the linker drops the library and AImageReader_* fail to load.
    #[link(name = "mediandk")]
    unsafe extern "C" {}

    #[link(name = "camera2ndk")]
    unsafe extern "C" {
        fn ACameraManager_create() -> *mut c_void;
        fn ACameraManager_delete(manager: *mut c_void);
        fn ACameraManager_getCameraIdList(manager: *mut c_void, list: *mut *mut IdList) -> i32;
        fn ACameraManager_deleteCameraIdList(list: *mut IdList);
        fn ACameraManager_getCameraCharacteristics(
            manager: *mut c_void,
            id: *const i8,
            metadata: *mut *mut c_void,
        ) -> i32;
        fn ACameraMetadata_getConstEntry(
            metadata: *const c_void,
            tag: u32,
            entry: *mut Entry,
        ) -> i32;
        fn ACameraMetadata_free(metadata: *mut c_void);
        fn ACameraManager_openCamera(
            manager: *mut c_void,
            id: *const i8,
            callbacks: *mut DeviceCallbacks,
            device: *mut *mut c_void,
        ) -> i32;
        fn ACameraDevice_close(device: *mut c_void) -> i32;
        fn ACameraDevice_createCaptureRequest(
            device: *const c_void,
            template: u32,
            request: *mut *mut c_void,
        ) -> i32;
        fn ACaptureRequest_setEntry_u8(
            request: *mut c_void,
            tag: u32,
            count: u32,
            data: *const u8,
        ) -> i32;
        fn ACaptureRequest_addTarget(request: *mut c_void, target: *const c_void) -> i32;
        fn ACaptureRequest_free(request: *mut c_void);
        fn ACameraOutputTarget_create(window: *mut c_void, target: *mut *mut c_void) -> i32;
        fn ACameraOutputTarget_free(target: *mut c_void);
        fn ACaptureSessionOutputContainer_create(container: *mut *mut c_void) -> i32;
        fn ACaptureSessionOutputContainer_free(container: *mut c_void);
        fn ACaptureSessionOutputContainer_add(container: *mut c_void, output: *const c_void)
        -> i32;
        fn ACaptureSessionOutput_create(window: *mut c_void, output: *mut *mut c_void) -> i32;
        fn ACaptureSessionOutput_free(output: *mut c_void);
        fn ACameraDevice_createCaptureSession(
            device: *mut c_void,
            outputs: *const c_void,
            callbacks: *const SessionCallbacks,
            session: *mut *mut c_void,
        ) -> i32;
        fn ACameraCaptureSession_capture(
            session: *mut c_void,
            callbacks: *mut c_void,
            count: i32,
            requests: *mut *mut c_void,
            sequence: *mut i32,
        ) -> i32;
        fn ACameraCaptureSession_close(session: *mut c_void);
    }

    #[repr(C)]
    struct IdList {
        count: i32,
        ids: *mut *const i8,
    }

    #[repr(C)]
    union EntryData {
        u8_: *const u8,
        i32_: *const i32,
    }

    #[repr(C)]
    struct Entry {
        tag: u32,
        type_: u8,
        count: u32,
        data: EntryData,
    }

    #[repr(C)]
    struct DeviceCallbacks {
        context: *mut c_void,
        on_disconnected: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
        on_error: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, i32)>,
    }

    #[repr(C)]
    struct SessionCallbacks {
        context: *mut c_void,
        on_closed: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
        on_ready: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
        on_active: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    }

    unsafe extern "C" fn on_disconnected(_: *mut c_void, _: *mut c_void) {}
    unsafe extern "C" fn on_error(_: *mut c_void, _: *mut c_void, _: i32) {}

    pub fn take_photo() -> Result<Photo, String> {
        if !permission_granted()? {
            request_permission()?;
            return Err(
                "Camera permission requested. Allow it, then tap Start Classifier again."
                    .to_string(),
            );
        }
        let jpeg = capture_jpeg()?;
        let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
            .map_err(|e| format!("could not decode camera JPEG: {e}"))?
            .to_rgba8();
        // The camera sensor delivers frames rotated 90° counter-clockwise on
        // this device, so rotate them clockwise before showing or classifying.
        let decoded = image::imageops::rotate90(&decoded);
        let (width, height) = decoded.dimensions();
        Ok(Photo {
            width,
            height,
            rgba: decoded.into_raw(),
        })
    }

    /// The `Activity` pointer, set once at startup. `ndk_context` holds the
    /// `Application`, which has no `requestPermissions`.
    static ACTIVITY: OnceLock<usize> = OnceLock::new();

    pub fn set_activity(ptr: *mut c_void) {
        let _ = ACTIVITY.set(ptr as usize);
    }

    fn with_activity<T>(
        f: impl FnOnce(&mut jni::Env, &JObject) -> Result<T, String>,
    ) -> Result<T, String> {
        let ptr = *ACTIVITY
            .get()
            .ok_or_else(|| "Android activity is not set".to_string())?;
        let ctx = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
        vm.attach_current_thread(|env| {
            let activity = unsafe { JObject::from_raw(env, ptr as *mut c_void as _) };
            Ok::<_, jni::errors::Error>(f(env, &activity))
        })
        .map_err(|e| e.to_string())?
    }

    fn permission_granted() -> Result<bool, String> {
        with_activity(|env, activity| {
            let name = env.new_string(PERMISSION).map_err(|e| e.to_string())?;
            let result = env
                .call_method(
                    activity,
                    jni::jni_str!("checkSelfPermission"),
                    jni::jni_sig!("(Ljava/lang/String;)I"),
                    &[JValue::Object(&name)],
                )
                .and_then(|v| v.i())
                .map_err(|e| e.to_string())?;
            Ok(result == 0)
        })
    }

    fn request_permission() -> Result<(), String> {
        with_activity(|env, activity| {
            let name = env.new_string(PERMISSION).map_err(|e| e.to_string())?;
            let array = env
                .new_object_array(1, jni::jni_str!("java/lang/String"), &name)
                .map_err(|e| e.to_string())?;
            env.call_method(
                activity,
                jni::jni_str!("requestPermissions"),
                jni::jni_sig!("([Ljava/lang/String;I)V"),
                &[JValue::Object(&array), JValue::Int(0)],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    fn capture_jpeg() -> Result<Vec<u8>, String> {
        unsafe {
            let manager = ACameraManager_create();
            if manager.is_null() {
                return Err("could not create the Android camera manager".to_string());
            }
            let result = capture_with(manager);
            ACameraManager_delete(manager);
            result
        }
    }

    unsafe fn capture_with(manager: *mut c_void) -> Result<Vec<u8>, String> {
        let id = unsafe { back_camera_id(manager)? };
        let (width, height) = unsafe { jpeg_size(manager, &id)? };

        let mut reader = std::ptr::null_mut();
        check(
            unsafe { ndk_sys::AImageReader_new(width, height, FORMAT_JPEG, 2, &mut reader).0 },
            "create ImageReader",
        )?;
        let result = unsafe { capture_into(manager, &id, reader) };
        unsafe { ndk_sys::AImageReader_delete(reader) };
        result
    }

    unsafe fn capture_into(
        manager: *mut c_void,
        id: &CStr,
        reader: *mut ndk_sys::AImageReader,
    ) -> Result<Vec<u8>, String> {
        let mut window = std::ptr::null_mut();
        check(
            unsafe { ndk_sys::AImageReader_getWindow(reader, &mut window).0 },
            "get ImageReader window",
        )?;

        let mut callbacks = DeviceCallbacks {
            context: std::ptr::null_mut(),
            on_disconnected: Some(on_disconnected),
            on_error: Some(on_error),
        };
        let mut device = std::ptr::null_mut();
        check(
            unsafe {
                ACameraManager_openCamera(manager, id.as_ptr().cast(), &mut callbacks, &mut device)
            },
            "open camera",
        )?;
        let result = unsafe { capture_on_device(device, window as *mut c_void, reader) };
        unsafe { ACameraDevice_close(device) };
        result
    }

    unsafe fn capture_on_device(
        device: *mut c_void,
        window: *mut c_void,
        reader: *mut ndk_sys::AImageReader,
    ) -> Result<Vec<u8>, String> {
        let mut target = std::ptr::null_mut();
        check(
            unsafe { ACameraOutputTarget_create(window, &mut target) },
            "create output target",
        )?;
        let mut request = std::ptr::null_mut();
        check(
            unsafe {
                ACameraDevice_createCaptureRequest(device, TEMPLATE_STILL_CAPTURE, &mut request)
            },
            "create still request",
        )?;
        check(
            unsafe { ACaptureRequest_addTarget(request, target) },
            "add target",
        )?;
        check(
            unsafe { ACaptureRequest_setEntry_u8(request, JPEG_QUALITY, 1, &JPEG_QUALITY_VALUE) },
            "set JPEG quality",
        )?;

        let mut container = std::ptr::null_mut();
        check(
            unsafe { ACaptureSessionOutputContainer_create(&mut container) },
            "create output container",
        )?;
        let mut output = std::ptr::null_mut();
        check(
            unsafe { ACaptureSessionOutput_create(window, &mut output) },
            "create session output",
        )?;
        check(
            unsafe { ACaptureSessionOutputContainer_add(container, output) },
            "add session output",
        )?;

        let callbacks = SessionCallbacks {
            context: std::ptr::null_mut(),
            on_closed: None,
            on_ready: None,
            on_active: None,
        };
        let mut session = std::ptr::null_mut();
        check(
            unsafe {
                ACameraDevice_createCaptureSession(device, container, &callbacks, &mut session)
            },
            "create capture session",
        )?;

        let captured = unsafe {
            let status = ACameraCaptureSession_capture(
                session,
                std::ptr::null_mut(),
                1,
                &mut request,
                std::ptr::null_mut(),
            );
            check(status, "capture still").and_then(|()| wait_for_jpeg(reader))
        };

        unsafe {
            ACameraCaptureSession_close(session);
            ACaptureSessionOutput_free(output);
            ACaptureSessionOutputContainer_free(container);
            ACaptureRequest_free(request);
            ACameraOutputTarget_free(target);
        }
        captured
    }

    /// Polls the reader until the still image arrives, then copies its JPEG bytes.
    unsafe fn wait_for_jpeg(reader: *mut ndk_sys::AImageReader) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let mut image = std::ptr::null_mut();
            let status = unsafe { ndk_sys::AImageReader_acquireNextImage(reader, &mut image).0 };
            if status == NO_IMAGE {
                if Instant::now() > deadline {
                    return Err("timed out waiting for the camera image".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            check(status, "acquire image")?;
            let result = unsafe { copy_jpeg(image) };
            unsafe { ndk_sys::AImage_delete(image) };
            return result;
        }
    }

    unsafe fn copy_jpeg(image: *mut ndk_sys::AImage) -> Result<Vec<u8>, String> {
        let mut data = std::ptr::null_mut();
        let mut len = 0;
        check(
            unsafe { ndk_sys::AImage_getPlaneData(image, 0, &mut data, &mut len).0 },
            "read JPEG plane",
        )?;
        if data.is_null() || len <= 0 {
            return Err("empty camera image".to_string());
        }
        Ok(unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec())
    }

    fn check(status: i32, operation: &str) -> Result<(), String> {
        if status == 0 {
            Ok(())
        } else {
            Err(format!("Android camera {operation} failed: {status}"))
        }
    }

    /// Finds the first back-facing camera.
    unsafe fn back_camera_id(manager: *mut c_void) -> Result<CString, String> {
        let mut list = std::ptr::null_mut();
        check(
            unsafe { ACameraManager_getCameraIdList(manager, &mut list) },
            "list cameras",
        )?;
        if list.is_null() {
            return Err("no camera list".to_string());
        }
        let mut found = None;
        for i in 0..unsafe { (*list).count.max(0) } as usize {
            let id = unsafe { *(*list).ids.add(i) };
            if id.is_null() {
                continue;
            }
            let mut metadata = std::ptr::null_mut();
            if unsafe { ACameraManager_getCameraCharacteristics(manager, id, &mut metadata) } != 0
                || metadata.is_null()
            {
                continue;
            }
            let mut facing = Entry {
                tag: 0,
                type_: 0,
                count: 0,
                data: EntryData {
                    u8_: std::ptr::null(),
                },
            };
            let is_back =
                unsafe { ACameraMetadata_getConstEntry(metadata, LENS_FACING, &mut facing) } == 0
                    && facing.count > 0
                    && unsafe {
                        !facing.data.u8_.is_null() && *facing.data.u8_ == LENS_FACING_BACK
                    };
            unsafe { ACameraMetadata_free(metadata) };
            if is_back {
                found = Some(unsafe { CStr::from_ptr(id.cast()) }.to_owned());
                break;
            }
        }
        unsafe { ACameraManager_deleteCameraIdList(list) };
        found.ok_or_else(|| "no back-facing camera found".to_string())
    }

    /// Largest JPEG output size whose longest side is at most `MAX_SIDE`.
    unsafe fn jpeg_size(manager: *mut c_void, id: &CStr) -> Result<(i32, i32), String> {
        let mut metadata = std::ptr::null_mut();
        check(
            unsafe {
                ACameraManager_getCameraCharacteristics(manager, id.as_ptr().cast(), &mut metadata)
            },
            "read camera characteristics",
        )?;
        let mut entry = Entry {
            tag: 0,
            type_: 0,
            count: 0,
            data: EntryData {
                i32_: std::ptr::null(),
            },
        };
        let status = unsafe { ACameraMetadata_getConstEntry(metadata, STREAM_CONFIGS, &mut entry) };
        let size = if status != 0 || entry.count == 0 || unsafe { entry.data.i32_.is_null() } {
            Err("no stream configurations".to_string())
        } else {
            let values =
                unsafe { std::slice::from_raw_parts(entry.data.i32_, entry.count as usize) };
            values
                .chunks_exact(4)
                .filter(|c| {
                    c[0] == FORMAT_JPEG && c[3] == STREAM_OUTPUT && c[1].max(c[2]) <= MAX_SIDE
                })
                .max_by_key(|c| c[1] * c[2])
                .map(|c| (c[1], c[2]))
                .ok_or_else(|| "no JPEG output size".to_string())
        };
        unsafe { ACameraMetadata_free(metadata) };
        size
    }
}
