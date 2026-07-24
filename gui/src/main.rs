//! Thin egui adapter over `rusty-clicker-core`. Build a `ClickPlan` from the
//! form, run it on a worker thread, drive the shared `AtomicBool` stop flag.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use rusty_clicker_core::{
    run, ClickPlan, EnigoClicker, Interval, MouseButton, RealSleeper, Repeat, Target,
};

const STOP_HOTKEY_LABEL: &str = "Ctrl+Shift+F6";

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
    status: Arc<Mutex<Status>>,

    // Kept alive so the registration stays active; polled in `ui`.
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
            status: Arc::new(Mutex::new(Status::Idle)),
            hotkey_manager: None,
            stop_hotkey: None,
        }
    }
}

impl App {
    fn is_running(&self) -> bool {
        matches!(*self.status.lock().unwrap(), Status::Running)
    }

    /// Build a plan, or return why the form is invalid.
    fn build_plan(&self) -> Result<ClickPlan, String> {
        let interval = if self.jitter_on {
            if self.jitter_max < self.interval_ms {
                return Err("jitter max must be >= interval".into());
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
        *self.status.lock().unwrap() = Status::Running;

        let stop = self.stop.clone();
        let status = self.status.clone();
        std::thread::spawn(move || {
            let mut clicker = match EnigoClicker::new() {
                Ok(c) => c,
                Err(e) => {
                    *status.lock().unwrap() = Status::Error(e);
                    return;
                }
            };
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

        ui.heading("rusty-clicker");
        ui.separator();

        ui.add_enabled_ui(!running, |ui| {
            egui::Grid::new("form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Interval (ms)");
                ui.add(egui::DragValue::new(&mut self.interval_ms).range(1..=u64::MAX));
                ui.end_row();

                ui.label("Jitter");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.jitter_on, "up to");
                    ui.add_enabled(
                        self.jitter_on,
                        egui::DragValue::new(&mut self.jitter_max).range(1..=u64::MAX),
                    );
                    ui.label("ms");
                });
                ui.end_row();

                ui.label("Button");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.button, MouseButton::Left, "Left");
                    ui.selectable_value(&mut self.button, MouseButton::Right, "Right");
                    ui.selectable_value(&mut self.button, MouseButton::Middle, "Middle");
                });
                ui.end_row();

                ui.label("Repeat");
                ui.vertical(|ui| {
                    ui.selectable_value(&mut self.repeat_mode, RepeatMode::Forever, "Forever");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.repeat_mode, RepeatMode::Times, "Times");
                        ui.add_enabled(
                            self.repeat_mode == RepeatMode::Times,
                            egui::DragValue::new(&mut self.times).range(1..=u64::MAX),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.repeat_mode, RepeatMode::Duration, "For");
                        ui.add_enabled(
                            self.repeat_mode == RepeatMode::Duration,
                            egui::DragValue::new(&mut self.duration_secs).range(1..=u64::MAX),
                        );
                        ui.label("s");
                    });
                });
                ui.end_row();

                ui.label("Target");
                ui.vertical(|ui| {
                    ui.selectable_value(&mut self.fixed_target, false, "Current cursor");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.fixed_target, true, "Fixed");
                        ui.add_enabled(self.fixed_target, egui::DragValue::new(&mut self.x).prefix("x "));
                        ui.add_enabled(self.fixed_target, egui::DragValue::new(&mut self.y).prefix("y "));
                    });
                });
                ui.end_row();
            });
        });

        ui.separator();
        ui.horizontal(|ui| {
            if running {
                if ui.button("Stop").clicked() {
                    self.stop();
                }
            } else if ui.button("Start").clicked() {
                self.start();
            }

            let status = self.status.lock().unwrap().clone();
            let (color, text) = match status {
                Status::Idle => (ui.visuals().weak_text_color(), "idle".to_string()),
                Status::Running => (egui::Color32::from_rgb(0x6F, 0xB9, 0x8F), "running…".to_string()),
                Status::Done(n) => (ui.visuals().text_color(), format!("done — {n} clicks")),
                Status::Error(e) => (egui::Color32::from_rgb(0xE0, 0x6C, 0x6C), format!("error: {e}")),
            };
            ui.colored_label(color, text);
        });

        if self.stop_hotkey.is_some() {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!("Global stop: {STOP_HOTKEY_LABEL}"))
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }

        // While a run is in flight, keep polling (also drives hotkey polling).
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }
}

/// Dark neutral theme: light text, thin borders, dark-pastel green
/// selection/press highlights. Forced dark so it ignores system mode.
fn setup_style(ctx: &egui::Context) {
    use egui::{Color32, CornerRadius, Stroke};

    let text = Color32::from_rgb(0xE6, 0xE6, 0xE6);
    let muted = Color32::from_rgb(0x9A, 0x9A, 0x9E);
    let panel = Color32::from_rgb(0x1F, 0x1F, 0x22);
    let surface = Color32::from_rgb(0x2A, 0x2A, 0x2E);
    let hover = Color32::from_rgb(0x34, 0x34, 0x3A);
    let border = Color32::from_rgb(0x3A, 0x3A, 0x40);
    let faint = Color32::from_rgb(0x2E, 0x2E, 0x33);
    let green = Color32::from_rgb(0x6F, 0xB9, 0x8F);
    let pastel = Color32::from_rgb(0x33, 0x51, 0x3E);

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(text);
    v.panel_fill = panel;
    v.window_fill = panel;
    v.extreme_bg_color = surface;
    v.selection.bg_fill = pastel;
    v.selection.stroke = Stroke::new(1.0, green);
    v.hyperlink_color = green;

    let r = CornerRadius::same(3);
    let w = &mut v.widgets;

    w.noninteractive.bg_fill = panel;
    w.noninteractive.weak_bg_fill = panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, faint);
    w.noninteractive.fg_stroke = Stroke::new(1.0, muted);

    w.inactive.bg_fill = surface;
    w.inactive.weak_bg_fill = surface;
    w.inactive.bg_stroke = Stroke::new(1.0, border);
    w.inactive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = hover;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x4A, 0x4A, 0x52));
    w.hovered.fg_stroke = Stroke::new(1.0, text);
    w.hovered.corner_radius = r;

    w.active.bg_fill = pastel;
    w.active.weak_bg_fill = pastel;
    w.active.bg_stroke = Stroke::new(1.0, green);
    w.active.fg_stroke = Stroke::new(1.0, text);
    w.active.corner_radius = r;

    w.open.bg_fill = surface;
    w.open.weak_bg_fill = surface;
    w.open.bg_stroke = Stroke::new(1.0, border);
    w.open.fg_stroke = Stroke::new(1.0, text);
    w.open.corner_radius = r;

    let mut style = egui::Style::default();
    style.visuals = v;
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([360.0, 340.0]),
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
