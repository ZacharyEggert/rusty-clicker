//! Thin egui adapter over `rusty-clicker-core`, styled as a small bench
//! instrument: a monospace readout (configured rate when idle, live click
//! count while running) over three labeled control blocks.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::path::PathBuf;

use eframe::egui;
use egui::{vec2, Color32, CornerRadius, RichText, Stroke};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use rusty_clicker_core::{
    run, ClickPlan, Clicker, EnigoClicker, Interval, MouseButton, RealSleeper, Repeat, Target,
};
use serde::{Deserialize, Serialize};

const DEFAULT_HOTKEY_LABEL: &str = "Ctrl+Shift+F6";

// --- Persistence ---------------------------------------------------------
// Settings live in the OS-conventional per-app config dir as config.toml:
//   macOS   ~/Library/Application Support/rusty-clicker/config.toml
//   Linux   ~/.config/rusty-clicker/config.toml
//   Windows %APPDATA%\rusty-clicker\config.toml

#[derive(Serialize, Deserialize)]
struct Config {
    interval_ms: u64,
    jitter_on: bool,
    jitter_max: u64,
    button: String,
    repeat_mode: String,
    times: u64,
    duration_secs: u64,
    fixed_target: bool,
    x: i32,
    y: i32,
    hotkey: Option<HotkeyConfig>,
}

#[derive(Serialize, Deserialize)]
struct HotkeyConfig {
    ctrl: bool,
    shift: bool,
    alt: bool,
    cmd: bool,
    code: String,
    label: String,
}

fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rusty-clicker")
        .map(|d| d.config_dir().join("config.toml"))
}

fn button_str(b: MouseButton) -> &'static str {
    match b {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}
fn button_from(s: &str) -> MouseButton {
    match s {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}
fn repeat_str(m: RepeatMode) -> &'static str {
    match m {
        RepeatMode::Forever => "forever",
        RepeatMode::Times => "times",
        RepeatMode::Duration => "duration",
    }
}
fn repeat_from(s: &str) -> RepeatMode {
    match s {
        "times" => RepeatMode::Times,
        "duration" => RepeatMode::Duration,
        _ => RepeatMode::Forever,
    }
}

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
    hotkey: Option<HotKey>,
    hotkey_combo: Option<(Modifiers, Code)>,
    hotkey_label: String,
    capturing: bool,
    hotkey_error: Option<String>,
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
            hotkey: None,
            hotkey_combo: None,
            hotkey_label: DEFAULT_HOTKEY_LABEL.to_string(),
            capturing: false,
            hotkey_error: None,
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

    fn toggle(&self) {
        if self.is_running() {
            self.stop();
        } else {
            self.start();
        }
    }

    /// Register a new toggle combo, releasing the old one only once the new one
    /// takes. On failure the old binding stays live and an error is surfaced.
    fn rebind(&mut self, code: Code, mods: Modifiers, label: String) {
        let Some(mgr) = &self.hotkey_manager else {
            return;
        };
        let new = HotKey::new((!mods.is_empty()).then_some(mods), code);
        if self.hotkey.map(|h| h.id()) == Some(new.id()) {
            self.hotkey_error = None; // same combo — nothing to do
            return;
        }
        match mgr.register(new) {
            Ok(()) => {
                if let Some(old) = self.hotkey.replace(new) {
                    let _ = mgr.unregister(old);
                }
                self.hotkey_combo = Some((mods, code));
                self.hotkey_label = label;
                self.hotkey_error = None;
                self.save_config();
            }
            Err(_) => {
                self.hotkey_error = Some(format!("{label} is already in use — try another"));
            }
        }
    }

    fn to_config(&self) -> Config {
        Config {
            interval_ms: self.interval_ms,
            jitter_on: self.jitter_on,
            jitter_max: self.jitter_max,
            button: button_str(self.button).to_string(),
            repeat_mode: repeat_str(self.repeat_mode).to_string(),
            times: self.times,
            duration_secs: self.duration_secs,
            fixed_target: self.fixed_target,
            x: self.x,
            y: self.y,
            hotkey: self.hotkey_combo.map(|(m, c)| HotkeyConfig {
                ctrl: m.contains(Modifiers::CONTROL),
                shift: m.contains(Modifiers::SHIFT),
                alt: m.contains(Modifiers::ALT),
                cmd: m.contains(Modifiers::SUPER),
                code: c.to_string(),
                label: self.hotkey_label.clone(),
            }),
        }
    }

    fn apply_config(&mut self, cfg: Config) {
        self.interval_ms = cfg.interval_ms;
        self.jitter_on = cfg.jitter_on;
        self.jitter_max = cfg.jitter_max;
        self.button = button_from(&cfg.button);
        self.repeat_mode = repeat_from(&cfg.repeat_mode);
        self.times = cfg.times;
        self.duration_secs = cfg.duration_secs;
        self.fixed_target = cfg.fixed_target;
        self.x = cfg.x;
        self.y = cfg.y;
        if let Some(hc) = cfg.hotkey {
            if let Ok(code) = hc.code.parse::<Code>() {
                let mut m = Modifiers::empty();
                if hc.ctrl {
                    m |= Modifiers::CONTROL;
                }
                if hc.shift {
                    m |= Modifiers::SHIFT;
                }
                if hc.alt {
                    m |= Modifiers::ALT;
                }
                if hc.cmd {
                    m |= Modifiers::SUPER;
                }
                self.rebind(code, m, hc.label);
            }
        }
    }

    /// Load saved settings, if any. Missing or unreadable config is fine.
    fn load_config(&mut self) {
        let Some(path) = config_path() else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        if let Ok(cfg) = toml::from_str::<Config>(&text) {
            self.apply_config(cfg);
        }
    }

    /// Best-effort save; a read-only config dir shouldn't crash the app.
    fn save_config(&self) {
        let Some(path) = config_path() else { return };
        let Ok(text) = toml::to_string_pretty(&self.to_config()) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

/// egui key -> global-hotkey code, via their shared variant names. Supports
/// letters, digits, F-keys, and the handful of named keys that round-trip.
fn egui_key_to_code(key: egui::Key) -> Option<Code> {
    let name = format!("{key:?}");
    let code = if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
        format!("Key{name}")
    } else if let Some(d) = name.strip_prefix("Num") {
        format!("Digit{d}")
    } else {
        name // F1.., Space, Arrow.., etc. share the Code names
    };
    code.parse().ok()
}

/// Human label for a captured key: "A", "6", "F6", "Space".
fn key_display(key: egui::Key) -> String {
    let name = format!("{key:?}");
    name.strip_prefix("Num").map(str::to_string).unwrap_or(name)
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
        // Drain global-hotkey events: the toggle works even when unfocused.
        if let Some(id) = self.hotkey.as_ref().map(|h| h.id()) {
            while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                if ev.id == id && ev.state == HotKeyState::Pressed {
                    self.toggle();
                }
            }
        }

        // Capture mode: bind the next real key press (Esc cancels).
        if self.capturing {
            let captured = ui.input(|i| {
                let key = i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, .. } => Some(*key),
                    _ => None,
                });
                (key, i.modifiers)
            });
            if let (Some(key), mods) = captured {
                self.capturing = false;
                if key != egui::Key::Escape {
                    if let Some(code) = egui_key_to_code(key) {
                        let mut parts: Vec<&str> = Vec::new();
                        if mods.ctrl {
                            parts.push("Ctrl");
                        }
                        if mods.shift {
                            parts.push("Shift");
                        }
                        if mods.alt {
                            parts.push("Alt");
                        }
                        if mods.mac_cmd {
                            parts.push("Cmd");
                        }
                        let disp = key_display(key);
                        parts.push(&disp);
                        let mut gm = Modifiers::empty();
                        if mods.ctrl {
                            gm |= Modifiers::CONTROL;
                        }
                        if mods.shift {
                            gm |= Modifiers::SHIFT;
                        }
                        if mods.alt {
                            gm |= Modifiers::ALT;
                        }
                        if mods.mac_cmd {
                            gm |= Modifiers::SUPER;
                        }
                        self.rebind(code, gm, parts.join("+"));
                    } else {
                        self.hotkey_error = Some("That key can't be used — try a letter, number, or F-key".into());
                    }
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

        // --- Hotkey (rebindable, works while running) --------------------
        block(ui, "HOTKEY", |ui| {
            ui.horizontal(|ui| {
                field_label(ui, "Start / stop");
                ui.label(
                    RichText::new(format!(" {} ", self.hotkey_label))
                        .monospace()
                        .size(12.0)
                        .color(TEXT)
                        .background_color(SURFACE),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.capturing {
                        if ui.button("Cancel").clicked() {
                            self.capturing = false;
                        }
                        ui.label(RichText::new("Press a combo…").size(11.0).color(ACCENT));
                    } else if ui.button("Change").clicked() {
                        self.capturing = true;
                        self.hotkey_error = None;
                    }
                });
            });
            if let Some(e) = &self.hotkey_error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).size(11.0).color(DANGER));
            } else {
                ui.add_space(4.0);
                ui.label(RichText::new("Works even when the window isn't focused").size(11.0).color(MUTED));
            }
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

        // Persist the final state (form edits included) as the window closes.
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            self.save_config();
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
            // Tall enough for the worst case (fixed-point + a stop sub-row both showing).
            .with_inner_size([380.0, 744.0])
            .with_min_inner_size([340.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rusty-clicker",
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            let mut app = App::default();
            // Best-effort: if the OS denies the default, the app still works via
            // the button and the user can bind a different combo.
            if let Ok(manager) = GlobalHotKeyManager::new() {
                let mods = Modifiers::CONTROL | Modifiers::SHIFT;
                let hotkey = HotKey::new(Some(mods), Code::F6);
                if manager.register(hotkey).is_ok() {
                    app.hotkey = Some(hotkey);
                    app.hotkey_combo = Some((mods, Code::F6));
                } else {
                    app.hotkey_error = Some("Ctrl+Shift+F6 is unavailable — set another".into());
                }
                app.hotkey_manager = Some(manager);
            }
            // Saved settings override the defaults (and rebind the hotkey).
            app.load_config();
            Ok(Box::new(app))
        }),
    )
}
