//! A tiny egui application that runs on Android via `cargo apk`.
//!
//! Continuously captures camera frames and lists the most likely SqueezeNet
//! ImageNet classes for each one. The latest frame and classification stay in
//! memory.

use eframe::egui;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

mod camera;
mod classifier;
mod squeezenet_fused;

/// Build-time QNN SDK root, if the crate was compiled with one.
const QNN_SDK_ROOT: Option<&str> = option_env!("QNN_SDK_ROOT");

/// Number of classes shown for each photo.
const TOP_K: usize = 4;

/// Edge length of the photo thumbnail in the UI.
const THUMBNAIL_SIZE: f32 = 140.0;

// ---------------------------------------------------------------------------
// Environment / QNN probes
// ---------------------------------------------------------------------------

/// Try to open the HTP backend the same way the burn-npu runtime does.
fn probe_qnn_runtime() -> Result<String, String> {
    // SAFETY: loading a shared library has no additional preconditions; the
    // handle is dropped immediately, before any symbol is called.
    match unsafe { libloading::Library::new("libQnnHtp.so") } {
        Ok(_) => Ok("libQnnHtp.so loaded".to_string()),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(target_os = "android")]
fn configure_adsp_library_path() {
    // The Hexagon DSP loader resolves `libQnnHtpV*Skel.so` through
    // ADSP_LIBRARY_PATH. Android installs native libs into an app-private
    // directory, so discover it from our own mapped library.
    let Some(dir) = native_lib_dir() else {
        log::warn!("could not locate the app's native library directory");
        return;
    };

    log::info!("ADSP_LIBRARY_PATH={}", dir.display());
    // SAFETY: called from `android_main` before any other thread is started.
    unsafe {
        std::env::set_var("ADSP_LIBRARY_PATH", &dir);
    }
    preload_cdsprpc();
}

/// The HTP stub links against `libcdsprpc.so` (FastRPC), which lives in the
/// vendor partition. The app's linker namespace does not resolve it by name,
/// so load it by absolute path with `RTLD_GLOBAL` first; later dependency
/// lookups then match the already-loaded soname.
#[cfg(target_os = "android")]
fn preload_cdsprpc() {
    const CANDIDATES: [&str; 2] = ["/vendor/lib64/libcdsprpc.so", "/system/lib64/libcdsprpc.so"];
    for path in CANDIDATES {
        if !std::path::Path::new(path).exists() {
            continue;
        }
        // SAFETY: loading the vendor FastRPC library has no preconditions here;
        // the handle is leaked so the library stays resident for the process.
        let result = unsafe {
            libloading::os::unix::Library::open(
                Some(path),
                libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
            )
        };
        match result {
            Ok(lib) => {
                log::info!("preloaded {path}");
                std::mem::forget(lib);
                return;
            }
            Err(err) => log::warn!("could not preload {path}: {err}"),
        }
    }
}

#[cfg(target_os = "android")]
fn native_lib_dir() -> Option<std::path::PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        for field in line.split_whitespace() {
            if field.ends_with("/libburn_phone.so") {
                return std::path::Path::new(field)
                    .parent()
                    .map(std::path::Path::to_path_buf);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct BurnPhoneApp {
    dark_mode: bool,

    qnn_compiled: bool,
    qnn_runtime: Result<String, String>,
    npu: Option<burn_npu::NpuInfo>,
    soc: String,

    photo: Option<PhotoView>,

    /// Classification of `photo`, filled in on each realtime frame.
    classes: Vec<classifier::Prediction>,
    /// NPU ops used for the last classification, or `None` before one runs.
    npu_ops: Option<u64>,

    /// Whether the realtime capture/classify loop is running.
    live_running: bool,
    live_stop: Option<Arc<AtomicBool>>,
    live_rx: Option<Receiver<Result<LiveFrame, String>>>,
    live_error: Option<String>,
}

/// A decoded photo ready for display.
struct PhotoView {
    width: u32,
    height: u32,
    texture: egui::TextureHandle,
}

/// One iteration of the realtime loop: a fresh photo and its classification.
struct LiveFrame {
    photo: camera::Photo,
    classification: classifier::Classification,
}

impl Default for BurnPhoneApp {
    fn default() -> Self {
        let npu = burn_npu::detect::detect();
        let soc = npu
            .as_ref()
            .map(|info| info.description.clone())
            .unwrap_or_else(|| {
                format!(
                    "No Qualcomm NPU detected ({}/{})",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )
            });

        Self {
            dark_mode: true,
            qnn_compiled: burn_npu::backends::qualcomm::qnn_compiled(),
            qnn_runtime: probe_qnn_runtime(),
            npu,
            soc,
            photo: None,
            classes: Vec::new(),
            npu_ops: None,
            live_running: false,
            live_stop: None,
            live_rx: None,
            live_error: None,
        }
    }
}

impl BurnPhoneApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let app = Self::default();
        cc.egui_ctx.set_visuals(if app.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        cc.egui_ctx.set_zoom_factor(1.25);
        app
    }

    /// Uploads a decoded photo to egui and makes it the visible preview.
    fn show_photo(&mut self, ctx: &egui::Context, photo: &camera::Photo) {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [photo.width as usize, photo.height as usize],
            &photo.rgba,
        );
        let texture = ctx.load_texture("photo", image, egui::TextureOptions::LINEAR);
        self.photo = Some(PhotoView {
            width: photo.width,
            height: photo.height,
            texture,
        });
    }

    /// Starts the realtime loop: capture a fresh photo, classify it, update the
    /// UI, wait about a second, and repeat until stopped.
    fn start_live(&mut self, ctx: &egui::Context) {
        if self.live_running {
            return;
        }

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        self.live_rx = Some(rx);
        self.live_stop = Some(stop.clone());
        self.live_running = true;
        self.live_error = None;

        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut next = Instant::now();
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let frame = camera::take_photo().map(|photo| {
                    let classification =
                        classifier::classify(&photo.rgba, photo.width, photo.height, TOP_K);
                    LiveFrame {
                        photo,
                        classification,
                    }
                });
                let failed = frame.is_err();
                if tx.send(frame).is_err() || failed {
                    break;
                }
                ctx.request_repaint();

                // Target one classification per second. If a capture takes
                // longer than that, start the next one immediately.
                next += Duration::from_secs(1);
                while Instant::now() < next {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            ctx.request_repaint();
        });
    }

    fn stop_live(&mut self) {
        // Let the worker finish its current capture and exit; the UI clears
        // `live_running` when the channel disconnects.
        if let Some(stop) = &self.live_stop {
            stop.store(true, Ordering::Relaxed);
        }
    }

    fn poll_live(&mut self, ctx: &egui::Context) {
        let mut items = Vec::new();
        let mut disconnected = false;
        let Some(rx) = &self.live_rx else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(item) => items.push(item),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        for item in items {
            match item {
                Ok(frame) => {
                    self.show_photo(ctx, &frame.photo);
                    self.classes = frame.classification.top;
                    self.npu_ops = Some(frame.classification.npu_ops);
                    self.live_error = None;
                }
                Err(err) => {
                    self.live_error = Some(err);
                    self.live_running = false;
                    self.live_rx = None;
                    self.live_stop = None;
                    return;
                }
            }
        }
        if disconnected {
            self.live_running = false;
            self.live_rx = None;
            self.live_stop = None;
        }
    }

    fn status_cell(ui: &mut egui::Ui, ok: bool) {
        let (color, mark) = if ok {
            (egui::Color32::from_rgb(80, 200, 120), "OK")
        } else {
            (egui::Color32::from_rgb(230, 110, 90), "!!")
        };
        ui.colored_label(color, mark);
    }

    fn environment_section(&self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Environment").strong());
        ui.add_space(6.0);

        let qnn_detail = match QNN_SDK_ROOT {
            Some(root) if self.qnn_compiled => format!("compiled in ({root})"),
            Some(_) => "bindgen did not run".to_string(),
            None => "not compiled in (set QNN_SDK_ROOT and rebuild)".to_string(),
        };
        let (runtime_ok, runtime_detail) = match &self.qnn_runtime {
            Ok(msg) => (true, msg.clone()),
            Err(err) => (false, format!("not loadable — {err}")),
        };

        egui::Grid::new("environment_grid")
            .num_columns(3)
            .striped(true)
            .max_col_width((ui.available_width() * 0.6).max(120.0))
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.strong("Component");
                ui.strong("Status");
                ui.strong("Details");
                ui.end_row();

                ui.label("QNN SDK");
                Self::status_cell(ui, self.qnn_compiled);
                ui.add(egui::Label::new(qnn_detail).wrap());
                ui.end_row();

                ui.label("Device");
                Self::status_cell(ui, self.npu.is_some());
                ui.add(egui::Label::new(&self.soc).wrap());
                ui.end_row();

                ui.label("HTP runtime");
                Self::status_cell(ui, runtime_ok);
                ui.add(egui::Label::new(runtime_detail).wrap());
                ui.end_row();
            });
    }

    fn photo_section(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Camera").strong());
        ui.add_space(6.0);

        let live_running = self.live_running;
        let label = if live_running {
            "Stop Classifier"
        } else {
            "Start Classifier"
        };
        if ui.add(egui::Button::new(label)).clicked() {
            if live_running {
                self.stop_live();
            } else {
                self.start_live(ui.ctx());
            }
        }

        if live_running {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Realtime: capturing and classifying about once per second.")
                    .small()
                    .weak(),
            );
            if self.photo.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for the first frame…");
                });
            }
        }
        if let Some(err) = &self.live_error {
            ui.add_space(6.0);
            ui.colored_label(egui::Color32::from_rgb(230, 110, 90), err);
        }

        if let Some(photo) = &self.photo {
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                let scale = THUMBNAIL_SIZE / photo.width.max(photo.height) as f32;
                let size = egui::vec2(photo.width as f32 * scale, photo.height as f32 * scale);
                ui.add(egui::Image::new(&photo.texture).fit_to_exact_size(size));

                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("Classification").strong());
                    ui.add_space(4.0);
                    if live_running && self.classes.is_empty() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Classifying…");
                        });
                    }
                    for class in &self.classes {
                        ui.horizontal(|ui| {
                            ui.label(class.label);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.monospace(format!("{:.1}%", class.probability * 100.0));
                                },
                            );
                        });
                    }
                    if let Some(ops) = self.npu_ops {
                        let text = if ops > 0 {
                            format!("Executed on: Hexagon NPU ({ops} ops)")
                        } else {
                            "Executed on: CPU".to_string()
                        };
                        ui.label(egui::RichText::new(text).small().weak());
                    }
                });
            });
            ui.label(
                egui::RichText::new(format!(
                    "{} × {}, kept in memory",
                    photo.width, photo.height
                ))
                .small()
                .weak(),
            );
        }
    }
}

impl eframe::App for BurnPhoneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_live(&ctx);
        if self.live_running {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        ui.ctx().set_visuals(if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(10.0);

                ui.vertical_centered(|ui| {
                    ui.heading("Burn Phone");
                    ui.label(
                        egui::RichText::new("SqueezeNet image classifier via burn")
                            .weak()
                            .small(),
                    );
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                self.photo_section(ui);

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);

                self.environment_section(ui);

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);

                ui.checkbox(&mut self.dark_mode, "Dark mode");

                ui.add_space(12.0);
            });
        });
    }
}

#[cfg(target_os = "android")]
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

#[cfg(target_os = "android")]
fn run_app(options: eframe::NativeOptions) -> eframe::Result {
    eframe::run_native(
        "Burn Phone",
        options,
        Box::new(|cc| Ok(Box::new(BurnPhoneApp::new(cc)))),
    )
}

/// Android entry point, called by `android-activity`'s `NativeActivity` glue.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("burn-phone"),
    );

    configure_adsp_library_path();
    camera::set_activity(app.activity_as_ptr());

    let mut options = native_options();
    options.android_app = Some(app);

    if let Err(err) = run_app(options) {
        log::error!("eframe failed to start: {err}");
    }
}
