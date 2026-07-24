//! Thin egui adapter over `rusty-clicker-core`, styled as a small bench
//! instrument: a monospace readout (configured rate when idle, live click
//! count while running) over three labeled control blocks.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use egui::{vec2, Color32, CornerRadius, RichText, Stroke};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use rusty_clicker_core::{
    run, ClickPlan, Clicker, EnigoClicker, Interval, MouseButton, RealSleeper, Repeat, Target,
};

const STOP_HOTKEY_LABEL: &str = "Ctrl+Shift+F6";

// --- Palette -------------------------------------------------------------
const BG: Color32 = Color32::from_rgb(0x16, 0x17, 0x1B);
const CARD: Color32 = Color32::from_rgb(0x20, 0x23, 0x29);
const READOUT_BG: Color32 = Color32::from_rgb(0x1B, 0x1D, 0x22);
const SURFACE: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x34);
const HOVER: Color32 = Color32::from_rgb(0x34, 0x38, 0x40);
const BORDER: Color32 = Color32::from_rgb(0x33, 0x37, 0x3F);
const TEXT: Color32 = Color32::from_rgb(0xE7, 0xE9, 0xEC);
const MUTED: Color32 = Color32::from_rgb(0x86, 0x8D, 0x96);
const ACCENT: Color32 = Color32::from_rgb(0x6F, 0xCF, 0x97);
const ACCENT_DIM: Color32 = Color32::from_rgb(0x2E, 0x4A, 0x39);
const DANGER: Color32 = Color32::from_rgb(0xE0, 0x6C, 0x6C);
const DANGER_DIM: Color32 = Color32::from_rgb(0x50, 0x2C, 0x30);

/// Counts successful clicks as they happen, sitting at the `Clicker` seam so
/// the GUI can show a live count without `core` knowing anything about it.
struct CountingClicker {
    inner: EnigoClicker,
    count: Arc<AtomicU64>,
}
impl Clicker for CountingClicker {
    fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
        self.inner.move_to(x, y)
    }
    fn click(&mut self, button: MouseButton) -> Result<(), String> {
        let r = self.inner.click(button);
        if r.is_ok() {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
        r
    }
}

#[derive(Clone)]
enum Status {
    Idle,
    Running,
    Done(u64),
    Error(String),
}

#[derive(PartialEq, Clone, Copy)]
enum RepeatMode {
    Forever,
    Times,
    Duration,
}

struct App {
    interval_ms: u64,
    jitter_on: bool,
    jitter_max: u64,
    button: MouseButton,
    repeat_mode: RepeatMode,
    times: u64,
    duration_secs: u64,
    fixed_target: bool,
    x: i32,
    y: i32,

    stop: Arc<AtomicBool>,
    count: Arc<AtomicU64>,
    status: Arc<Mutex<Status>>,

    hotkey_manager: Option<GlobalHotKeyManager>,
    stop_hotkey: Option<HotKey>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            interval_ms: 100,
            jitter_on: false,
            jitter_max: 150,
            button: MouseButton::Left,
            repeat_mode: RepeatMode::Forever,
            times: 100,
            duration_secs: 10,
            fixed_target: false,
            x: 0,
            y: 0,
            stop: Arc::new(AtomicBool::new(false)),
            count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(Status::Idle)),
            hotkey_manager: None,
            stop_hotkey: None,
        }
    }
}

impl App {
    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn is_running(&self) -> bool {
        matches!(self.status(), Status::Running)
    }

    /// Average clicks per second for the current settings (0 if not clickable).
    fn rate_per_sec(&self) -> f64 {
        let avg_ms = if self.jitter_on && self.jitter_max >= self.interval_ms {
            (self.interval_ms + self.jitter_max) as f64 / 2.0
        } else {
            self.interval_ms as f64
        };
        if avg_ms <= 0.0 {
            0.0
        } else {
            1000.0 / avg_ms
        }
    }

    fn build_plan(&self) -> Result<ClickPlan, String> {
        let interval = if self.jitter_on {
            if self.jitter_max < self.interval_ms {
                return Err("Jitter max must be at least the interval".into());
            }
            Interval::Uniform(
                Duration::from_millis(self.interval_ms),
                Duration::from_millis(self.jitter_max),
            )
        } else {
            Interval::Fixed(Duration::from_millis(self.interval_ms))
        };
        let repeat = match self.repeat_mode {
            RepeatMode::Forever => Repeat::Forever,
            RepeatMode::Times => Repeat::Times(self.times),
            RepeatMode::Duration => Repeat::For(Duration::from_secs(self.duration_secs)),
        };
        let target = if self.fixed_target {
            Target::Fixed { x: self.x, y: self.y }
        } else {
            Target::CurrentCursor
        };
        Ok(ClickPlan {
            button: self.button,
            interval,
            repeat,
            target,
        })
    }

    fn start(&self) {
        let plan = match self.build_plan() {
            Ok(p) => p,
            Err(e) => {
                *self.status.lock().unwrap() = Status::Error(e);
                return;
            }
        };
        self.stop.store(false, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
        *self.status.lock().unwrap() = Status::Running;

        let stop = self.stop.clone();
        let status = self.status.clone();
        let count = self.count.clone();
        std::thread::spawn(move || {
            let inner = match EnigoClicker::new() {
                Ok(c) => c,
                Err(e) => {
                    *status.lock().unwrap() = Status::Error(e);
                    return;
                }
            };
            let mut clicker = CountingClicker { inner, count };
            let mut rng = rand::thread_rng();
            let result = run(&plan, &mut clicker, &RealSleeper, &mut rng, &stop);
            *status.lock().unwrap() = match result {
                Ok(n) => Status::Done(n),
                Err(e) => Status::Error(e),
            };
        });
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A labeled control block with a small uppercase caption.
fn block(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(11.0).strong().color(MUTED));
            ui.add_space(8.0);
            add(ui);
        });
    ui.add_space(10.0);
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(MUTED).size(12.0));
}

/// A small filled status dot, painted (not a glyph, which the default font lacks).
fn led(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 9.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Drain global-hotkey events: the stop hotkey works even when unfocused.
        if let Some(stop_id) = self.stop_hotkey.as_ref().map(|h| h.id()) {
            while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                if ev.id == stop_id && ev.state == HotKeyState::Pressed {
                    self.stop();
                }
            }
        }

        let running = self.is_running();
        let status = self.status();

        // --- Nameplate ---------------------------------------------------
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("RUSTY CLICKER").size(15.0).strong().color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (dot, word) = match status {
                    Status::Idle => (MUTED, "Ready"),
                    Status::Running => (ACCENT, "Running"),
                    Status::Done(_) => (ACCENT, "Done"),
                    Status::Error(_) => (DANGER, "Error"),
                };
                ui.add_space(4.0); // keep the indicator off the right frame edge
                ui.label(RichText::new(word).size(11.0).color(MUTED));
                ui.add_space(2.0);
                led(ui, dot);
            });
        });
        ui.add_space(10.0);

        // --- Readout (hero) ---------------------------------------------
        let live = self.count.load(Ordering::Relaxed);
        let (number, unit, glow) = match &status {
            Status::Running => (live.to_string(), "clicks so far", ACCENT),
            Status::Done(n) => (n.to_string(), "clicks — done", ACCENT),
            Status::Error(_) => ("—".to_string(), "check the message below", DANGER),
            Status::Idle => (format!("{:.1}", self.rate_per_sec()), "clicks / second", ACCENT),
        };
        egui::Frame::default()
            .fill(READOUT_BG)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(number).monospace().size(42.0).strong().color(glow));
                ui.label(RichText::new(unit).size(12.0).color(MUTED));
            });
        ui.add_space(10.0);

        // --- Controls (disabled while running) --------------------------
        ui.add_enabled_ui(!running, |ui| {
            block(ui, "TIMING", |ui| {
                ui.horizontal(|ui| {
                    field_label(ui, "Interval");
                    ui.add(egui::DragValue::new(&mut self.interval_ms).range(1..=u64::MAX).suffix(" ms"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = self.rate_per_sec();
                        ui.label(RichText::new(format!("≈ {r:.1}/s")).monospace().size(11.0).color(ACCENT));
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.jitter_on, "");
                    field_label(ui, "Randomize up to");
                    ui.add_enabled(
                        self.jitter_on,
                        egui::DragValue::new(&mut self.jitter_max).range(1..=u64::MAX).suffix(" ms"),
                    );
                });
                if self.jitter_on && self.jitter_max < self.interval_ms {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Max is below the interval — no jitter applied").size(11.0).color(DANGER));
                }
            });

            block(ui, "CLICK", |ui| {
                ui.horizontal(|ui| {
                    field_label(ui, "Button");
                    ui.selectable_value(&mut self.button, MouseButton::Left, "Left");
                    ui.selectable_value(&mut self.button, MouseButton::Right, "Right");
                    ui.selectable_value(&mut self.button, MouseButton::Middle, "Middle");
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    field_label(ui, "Where");
                    ui.selectable_value(&mut self.fixed_target, false, "Cursor");
                    ui.selectable_value(&mut self.fixed_target, true, "Fixed point");
                });
                if self.fixed_target {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        field_label(ui, "Point");
                        ui.add(egui::DragValue::new(&mut self.x).prefix("x "));
                        ui.add(egui::DragValue::new(&mut self.y).prefix("y "));
                    });
                }
            });

            block(ui, "STOP", |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.repeat_mode, RepeatMode::Forever, "Until stopped");
                    ui.selectable_value(&mut self.repeat_mode, RepeatMode::Times, "A set count");
                    ui.selectable_value(&mut self.repeat_mode, RepeatMode::Duration, "For a time");
                });
                match self.repeat_mode {
                    RepeatMode::Forever => {}
                    RepeatMode::Times => {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            field_label(ui, "Clicks");
                            ui.add(egui::DragValue::new(&mut self.times).range(1..=u64::MAX));
                        });
                    }
                    RepeatMode::Duration => {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            field_label(ui, "Seconds");
                            ui.add(egui::DragValue::new(&mut self.duration_secs).range(1..=u64::MAX));
                        });
                    }
                }
            });
        });

        // --- Error detail ------------------------------------------------
        if let Status::Error(e) = &status {
            ui.label(RichText::new(e).size(12.0).color(DANGER));
            ui.add_space(8.0);
        }

        // --- Action key --------------------------------------------------
        let w = ui.available_width();
        if running {
            let btn = egui::Button::new(RichText::new("Stop").size(15.0).strong().color(TEXT))
                .fill(DANGER_DIM)
                .stroke(Stroke::new(1.0, DANGER))
                .corner_radius(CornerRadius::same(6));
            if ui.add_sized(vec2(w, 42.0), btn).clicked() {
                self.stop();
            }
        } else {
            let btn = egui::Button::new(RichText::new("Start").size(15.0).strong().color(BG))
                .fill(ACCENT)
                .corner_radius(CornerRadius::same(6));
            if ui.add_sized(vec2(w, 42.0), btn).clicked() {
                self.start();
            }
        }

        // --- Footer ------------------------------------------------------
        if self.stop_hotkey.is_some() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!("{STOP_HOTKEY_LABEL} stops from anywhere"))
                        .size(11.0)
                        .color(MUTED),
                );
            });
            ui.add_space(4.0);
        }

        // While a run is in flight, keep repainting so the live count and
        // hotkey events keep flowing.
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
    }
}

/// Dark bench-instrument theme. Forced dark so it ignores system mode.
fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = SURFACE;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;

    let r = CornerRadius::same(4);
    let w = &mut v.widgets;

    w.noninteractive.bg_fill = BG;
    w.noninteractive.weak_bg_fill = BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);

    w.inactive.bg_fill = SURFACE;
    w.inactive.weak_bg_fill = SURFACE;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = HOVER;
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = r;

    w.active.bg_fill = ACCENT_DIM;
    w.active.weak_bg_fill = ACCENT_DIM;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.corner_radius = r;

    w.open.bg_fill = SURFACE;
    w.open.weak_bg_fill = SURFACE;
    w.open.bg_stroke = Stroke::new(1.0, BORDER);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.corner_radius = r;

    let mut style = egui::Style::default();
    style.visuals = v;
    style.spacing.button_padding = vec2(10.0, 5.0);
    style.spacing.item_spacing = vec2(8.0, 6.0);
    style.spacing.interact_size.y = 24.0;
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 612.0])
            .with_min_inner_size([340.0, 580.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rusty-clicker",
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            let mut app = App::default();
            // Best-effort: if the OS denies the hotkey, the app still works via the button.
            if let Ok(manager) = GlobalHotKeyManager::new() {
                let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::F6);
                if manager.register(hotkey).is_ok() {
                    app.stop_hotkey = Some(hotkey);
                    app.hotkey_manager = Some(manager);
                }
            }
            Ok(Box::new(app))
        }),
    )
}
