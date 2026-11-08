use crate::config::{self, Action, ClickMethod, Config, Macro, Mode, Step, K};
use crate::cursor;
use crate::engine::{Capture, Shared};
use crate::keys;
use crate::typing::{self, Layout};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

const GREEN: Color32 = Color32::from_rgb(46, 160, 67);
const GREY: Color32 = Color32::from_rgb(90, 90, 96);
const WARN: Color32 = Color32::from_rgb(230, 170, 60);
const RED: Color32 = Color32::from_rgb(220, 80, 80);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Macros,
    Settings,
    Status,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Bind {
    Toggle,
    RecordStop,
    Trigger(usize),
    Modifier(usize),
    RepeatKey(usize),
    RemapTo(usize),
    StepKey(usize, usize),
    TypeInsert(usize),
    ClickPos(usize, usize),
    TypeClick(usize),
}

impl Bind {
    fn is_position(self) -> bool {
        matches!(self, Bind::ClickPos(..) | Bind::TypeClick(_))
    }

    fn clearable(self) -> bool {
        matches!(self, Bind::Toggle | Bind::RecordStop | Bind::Trigger(_))
    }
}

enum Req {
    Bind(Bind),
    RecStart,
    RecStop,
}

pub struct App {
    sh: Arc<Shared>,
    cfg: Config,
    applied: Config,
    path: PathBuf,
    dirty: Option<Instant>,
    save_err: Option<String>,
    sel: usize,
    tab: Tab,
    bind: Option<Bind>,
    filter: String,
    capture_left: bool,
    recording_for: Option<usize>,
    undo: Option<(usize, Macro, Instant)>,
    layout: Arc<Layout>,
    layout_spec: String,
}

pub fn run(sh: Arc<Shared>, cfg: Config, path: PathBuf) -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("evmacro")
            .with_app_id("evmacro")
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "evmacro",
        opts,
        Box::new(move |cc| {
            let cfg_layout = cfg.keyboard_layout.clone();
            cc.egui_ctx.style_mut(|s| {
                s.spacing.item_spacing = egui::vec2(8.0, 6.0);
                s.spacing.button_padding = egui::vec2(8.0, 4.0);
            });
            Ok(Box::new(App {
                sh,
                applied: cfg.clone(),
                cfg,
                path,
                dirty: None,
                save_err: None,
                sel: 0,
                tab: Tab::Macros,
                bind: None,
                filter: String::new(),
                capture_left: false,
                recording_for: None,
                undo: None,
                layout: typing::layout(&cfg_layout),
                layout_spec: cfg_layout,
            }))
        }),
    )
}

impl Drop for App {
    fn drop(&mut self) {
        if self.dirty.is_some() {
            let _ = config::save(&self.path, &self.cfg);
        }
    }
}

fn key_text(k: Option<K>) -> String {
    k.map(|k| keys::pretty(k.0)).unwrap_or_else(|| "— none —".into())
}

fn key_btn(ui: &mut egui::Ui, k: Option<K>) -> bool {
    ui.add(egui::Button::new(RichText::new(key_text(k)).monospace()).min_size(egui::vec2(96.0, 0.0)))
        .on_hover_text("Click, then press a key or mouse button")
        .clicked()
}

fn chips(ui: &mut egui::Ui, list: &mut Vec<K>) -> bool {
    let mut add = false;
    ui.horizontal_wrapped(|ui| {
        let mut remove = None;
        for (i, k) in list.iter().enumerate() {
            if ui
                .add(egui::Button::new(
                    RichText::new(format!("{}  ✕", keys::pretty(k.0))).monospace(),
                ))
                .on_hover_text("Remove")
                .clicked()
            {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            list.remove(i);
        }
        add = ui.button("＋ Add").clicked();
    });
    add
}

fn summary(m: &Macro) -> String {
    let what = match &m.action {
        Action::Repeat { interval_ms, .. } => format!("Repeat {:.0}/s", 1000.0 / interval_ms.max(0.05)),
        Action::Sequence { steps, .. } => format!("Sequence ({})", steps.len()),
        Action::Remap { .. } => "Remap".into(),
        Action::Type { text, .. } => {
            let t: String = text.chars().take(18).collect();
            format!("Type \"{t}{}\"", if text.chars().count() > 18 { "…" } else { "" })
        }
    };
    let mode = match (&m.action, m.mode) {
        (Action::Remap { .. }, _) => "",
        (_, Mode::Hold) => "Hold · ",
        (_, Mode::Toggle) => "Toggle · ",
    };
    format!("{mode}{} → {what}", key_text(m.trigger))
}

fn step_kind(s: &Step) -> &'static str {
    match s {
        Step::Tap { .. } => "Tap",
        Step::Down { .. } => "Down",
        Step::Up { .. } => "Up",
        Step::Wait { .. } => "Wait",
        Step::Move { .. } => "Move",
        Step::Wheel { .. } => "Wheel",
        Step::HWheel { .. } => "HWheel",
        Step::Type { .. } => "Type",
        Step::Click { .. } => "Click",
    }
}

fn step_key(s: &Step) -> Option<K> {
    match s {
        Step::Tap { key, .. } | Step::Down { key } | Step::Up { key } => Some(*key),
        _ => None,
    }
}

fn new_step(kind: &str, key: K) -> Step {
    match kind {
        "Tap" => Step::Tap { key, hold_ms: 10.0 },
        "Down" => Step::Down { key },
        "Up" => Step::Up { key },
        "Wait" => Step::Wait { ms: 50.0 },
        "Move" => Step::Move { dx: 0, dy: 0 },
        "Wheel" => Step::Wheel { amount: -1 },
        "Type" => Step::Type {
            text: String::new(),
            char_ms: 8.0,
        },
        "Click" => Step::Click {
            x: 0,
            y: 0,
            button: K(0x110),
            method: ClickMethod::Pointer,
            back: true,
        },
        _ => Step::HWheel { amount: 1 },
    }
}

const STEP_KINDS: [&str; 9] = ["Tap", "Down", "Up", "Wait", "Type", "Click", "Move", "Wheel", "HWheel"];

const HOW: [&str; 3] = ["Jump & return", "Move there", "Touch tap"];

fn click_editor(
    ui: &mut egui::Ui,
    id: (usize, usize),
    x: &mut i32,
    y: &mut i32,
    button: &mut K,
    method: &mut ClickMethod,
    back: &mut bool,
) -> bool {
    ui.label("x");
    ui.add(egui::DragValue::new(x).range(-100_000..=100_000));
    ui.label("y");
    ui.add(egui::DragValue::new(y).range(-100_000..=100_000));
    let pick = ui
        .button("📍 Pick")
        .on_hover_text("Hover the mouse over the spot, then press any key")
        .clicked();
    let cur = match (*method, *back) {
        (ClickMethod::Touch, _) => 2,
        (ClickMethod::Pointer, true) => 0,
        (ClickMethod::Pointer, false) => 1,
    };
    let mut how = cur;
    egui::ComboBox::from_id_salt(("how", id.0, id.1))
        .selected_text(HOW[cur])
        .width(120.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut how, 0, HOW[0])
                .on_hover_text("Cursor jumps there, clicks, and comes back (works everywhere)");
            ui.selectable_value(&mut how, 1, HOW[1])
                .on_hover_text("Cursor moves there and clicks");
            ui.selectable_value(&mut how, 2, HOW[2])
                .on_hover_text("Touchscreen tap: the cursor never moves (app must accept touch)");
        });
    if how != cur {
        (*method, *back) = match how {
            0 => (ClickMethod::Pointer, true),
            1 => (ClickMethod::Pointer, false),
            _ => (ClickMethod::Touch, true),
        };
    }
    if *method == ClickMethod::Pointer {
        egui::ComboBox::from_id_salt(("btn", id.0, id.1))
            .selected_text(match button.0 {
                0x111 => "Right",
                0x112 => "Middle",
                _ => "Left",
            })
            .width(70.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(button, K(0x110), "Left");
                ui.selectable_value(button, K(0x111), "Right");
                ui.selectable_value(button, K(0x112), "Middle");
            });
    }
    pick
}

#[allow(clippy::too_many_arguments)]
fn macro_editor(
    ui: &mut egui::Ui,
    m: &mut Macro,
    i: usize,
    exclusive: bool,
    recording: bool,
    lay: &Layout,
    conflicts: &[String],
    req: &mut Option<Req>,
    delete: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.heading(&m.name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(RichText::new("🗑 Delete macro").color(RED)))
                .clicked()
            {
                *delete = true;
            }
        });
    });
    ui.separator();
    let is_remap = matches!(m.action, Action::Remap { .. });

    egui::Grid::new("meta")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(320.0));
            ui.end_row();

            ui.label("Enabled");
            ui.checkbox(&mut m.enabled, "");
            ui.end_row();

            ui.label("Trigger");
            ui.horizontal(|ui| {
                if key_btn(ui, m.trigger) {
                    *req = Some(Req::Bind(Bind::Trigger(i)));
                }
                if !conflicts.is_empty() {
                    ui.colored_label(WARN, format!("⚠ same hotkey as: {}", conflicts.join(", ")))
                        .on_hover_text("Both macros will run at once. Give one of them a modifier (e.g. Ctrl).");
                }
            });
            ui.end_row();

            ui.label("Modifiers")
                .on_hover_text("Keys that must already be held when the trigger is pressed");
            if chips(ui, &mut m.modifiers) {
                *req = Some(Req::Bind(Bind::Modifier(i)));
            }
            ui.end_row();

            if matches!(m.action, Action::Type { .. }) {
                ui.label("Mode");
                ui.weak("Tap the trigger to type · tap again to cancel");
                ui.end_row();
            } else if !is_remap {
                ui.label("Mode");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut m.mode, Mode::Hold, "Hold")
                        .on_hover_text("Runs while the trigger is held");
                    ui.radio_value(&mut m.mode, Mode::Toggle, "Toggle")
                        .on_hover_text("Press to start, press again to stop");
                });
                ui.end_row();

                ui.label("Block trigger");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut m.block_trigger, "Don't send the trigger key to apps");
                    if m.block_trigger && !exclusive {
                        ui.colored_label(WARN, "⚠ needs Exclusive input (Settings)");
                    }
                });
                ui.end_row();
            }

            ui.label("Action");
            const NAMES: [&str; 4] = ["Repeat (autoclick / spam)", "Sequence", "Type text", "Remap"];
            let cur = match m.action {
                Action::Repeat { .. } => 0,
                Action::Sequence { .. } => 1,
                Action::Type { .. } => 2,
                Action::Remap { .. } => 3,
            };
            let mut pick = cur;
            egui::ComboBox::from_id_salt(("action", i))
                .selected_text(NAMES[cur])
                .width(220.0)
                .show_ui(ui, |ui| {
                    for (n, name) in NAMES.iter().enumerate() {
                        ui.selectable_value(&mut pick, n, *name);
                    }
                });
            if pick != cur {
                m.action = match pick {
                    0 => Action::default(),
                    1 => Action::Sequence {
                        steps: vec![],
                        speed: 1.0,
                        count: 1,
                    },
                    2 => {
                        m.mode = Mode::Toggle;
                        Action::Type {
                            text: String::new(),
                            char_ms: 8.0,
                            hold_ms: 8.0,
                            settle_ms: 50.0,
                            count: 1,
                        }
                    }
                    _ => Action::Remap { to: vec![] },
                };
            }
            ui.end_row();
        });

    ui.separator();

    match &mut m.action {
        Action::Repeat {
            keys,
            interval_ms,
            jitter_ms,
            press_ms,
            count,
        } => {
            egui::Grid::new("repeat")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Output keys");
                    if chips(ui, keys) {
                        *req = Some(Req::Bind(Bind::RepeatKey(i)));
                    }
                    ui.end_row();

                    ui.label("Interval");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(interval_ms)
                                .range(0.1..=600_000.0)
                                .speed(0.2)
                                .suffix(" ms")
                                .max_decimals(2),
                        );
                        let mut cps = 1000.0 / interval_ms.max(0.05);
                        ui.label("=");
                        if ui
                            .add(
                                egui::DragValue::new(&mut cps)
                                    .range(0.01..=10_000.0)
                                    .speed(0.5)
                                    .suffix(" /s")
                                    .max_decimals(2),
                            )
                            .changed()
                        {
                            *interval_ms = 1000.0 / cps.max(0.01);
                        }
                    });
                    ui.end_row();

                    ui.label("Press duration")
                        .on_hover_text("How long each key is held down. Some games ignore 0 ms presses.");
                    ui.add(
                        egui::DragValue::new(press_ms)
                            .range(0.0..=*interval_ms)
                            .speed(0.1)
                            .suffix(" ms")
                            .max_decimals(2),
                    );
                    ui.end_row();

                    ui.label("Random jitter ±")
                        .on_hover_text("Randomizes each interval by up to this amount");
                    ui.add(
                        egui::DragValue::new(jitter_ms)
                            .range(0.0..=10_000.0)
                            .speed(0.1)
                            .suffix(" ms")
                            .max_decimals(2),
                    );
                    ui.end_row();

                    ui.label("Repeat count");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(count).range(0..=1_000_000));
                        ui.weak("0 = until stopped");
                    });
                    ui.end_row();
                });
        }
        Action::Sequence { steps, speed, count } => {
            ui.horizontal(|ui| {
                ui.label("Speed");
                ui.add(egui::DragValue::new(speed).range(0.05..=50.0).speed(0.01).suffix("×"));
                ui.separator();
                ui.label("Loops");
                ui.add(egui::DragValue::new(count).range(0..=1_000_000));
                ui.weak("0 = until stopped");
                ui.separator();
                if recording {
                    if ui
                        .add(egui::Button::new(RichText::new("■ Stop recording").color(Color32::WHITE)).fill(RED))
                        .clicked()
                    {
                        *req = Some(Req::RecStop);
                    }
                } else if ui
                    .button("● Record")
                    .on_hover_text("Records your keys/clicks with timing. Replaces the steps.")
                    .clicked()
                {
                    *req = Some(Req::RecStart);
                }
                if ui.button("Clear").clicked() {
                    steps.clear();
                }
            });
            let total: f64 = steps
                .iter()
                .map(|s| match s {
                    Step::Wait { ms } => *ms,
                    Step::Tap { hold_ms, .. } => *hold_ms,
                    _ => 0.0,
                })
                .sum::<f64>()
                / speed.max(0.05);
            ui.weak(format!("{} steps · {:.1} ms per loop", steps.len(), total));

            let mut action: Option<(usize, i8)> = None;
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Grid::new("steps")
                        .num_columns(4)
                        .striped(true)
                        .spacing([10.0, 4.0])
                        .show(ui, |ui| {
                            for (si, st) in steps.iter_mut().enumerate() {
                                ui.weak(format!("{:>3}", si + 1));
                                let kind = step_kind(st);
                                let mut nk = kind;
                                egui::ComboBox::from_id_salt(("op", i, si))
                                    .selected_text(kind)
                                    .width(80.0)
                                    .show_ui(ui, |ui| {
                                        for k in STEP_KINDS {
                                            ui.selectable_value(&mut nk, k, k);
                                        }
                                    });
                                if nk != kind {
                                    *st = new_step(nk, step_key(st).unwrap_or(K(30)));
                                }
                                ui.horizontal(|ui| match st {
                                    Step::Tap { key, hold_ms } => {
                                        if key_btn(ui, Some(*key)) {
                                            *req = Some(Req::Bind(Bind::StepKey(i, si)));
                                        }
                                        ui.label("hold");
                                        ui.add(
                                            egui::DragValue::new(hold_ms)
                                                .range(0.0..=600_000.0)
                                                .speed(0.2)
                                                .suffix(" ms"),
                                        );
                                    }
                                    Step::Down { key } | Step::Up { key } => {
                                        if key_btn(ui, Some(*key)) {
                                            *req = Some(Req::Bind(Bind::StepKey(i, si)));
                                        }
                                    }
                                    Step::Wait { ms } => {
                                        ui.add(
                                            egui::DragValue::new(ms)
                                                .range(0.0..=3_600_000.0)
                                                .speed(0.5)
                                                .suffix(" ms"),
                                        );
                                    }
                                    Step::Move { dx, dy } => {
                                        ui.label("dx");
                                        ui.add(egui::DragValue::new(dx).range(-10_000..=10_000));
                                        ui.label("dy");
                                        ui.add(egui::DragValue::new(dy).range(-10_000..=10_000));
                                    }
                                    Step::Wheel { amount } | Step::HWheel { amount } => {
                                        ui.add(egui::DragValue::new(amount).range(-100..=100));
                                        ui.weak("notches");
                                    }
                                    Step::Click {
                                        x,
                                        y,
                                        button,
                                        method,
                                        back,
                                    } => {
                                        if click_editor(ui, (i, si), x, y, button, method, back) {
                                            *req = Some(Req::Bind(Bind::ClickPos(i, si)));
                                        }
                                    }
                                    Step::Type { text, char_ms } => {
                                        ui.add(
                                            egui::TextEdit::singleline(text)
                                                .hint_text("text, {Enter}, {Ctrl+V}…")
                                                .desired_width(220.0),
                                        );
                                        ui.add(
                                            egui::DragValue::new(char_ms)
                                                .range(0.0..=10_000.0)
                                                .speed(0.1)
                                                .suffix(" ms/key"),
                                        );
                                        if let Err(e) = typing::parse(text) {
                                            ui.colored_label(RED, "⚠").on_hover_text(e);
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    if ui.small_button("⬆").clicked() {
                                        action = Some((si, -1));
                                    }
                                    if ui.small_button("⬇").clicked() {
                                        action = Some((si, 1));
                                    }
                                    if ui.small_button("✕").clicked() {
                                        action = Some((si, 0));
                                    }
                                });
                                ui.end_row();
                            }
                        });
                });
            match action {
                Some((si, 0)) => {
                    steps.remove(si);
                }
                Some((si, -1)) if si > 0 => steps.swap(si, si - 1),
                Some((si, 1)) if si + 1 < steps.len() => steps.swap(si, si + 1),
                _ => {}
            }
            ui.horizontal(|ui| {
                ui.label("Add:");
                for k in STEP_KINDS {
                    if ui.button(k).clicked() {
                        let key = steps.iter().rev().find_map(step_key).unwrap_or(K(30));
                        steps.push(new_step(k, key));
                    }
                }
            });
        }
        Action::Type {
            text,
            char_ms,
            hold_ms,
            settle_ms,
            count,
        } => {
            ui.label("Text to type");
            ui.add(
                egui::TextEdit::multiline(text)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(5)
                    .desired_width(f32::INFINITY)
                    .hint_text("Hello world{Enter}"),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label("Insert:");
                for tok in ["{Enter}", "{Tab}", "{Backspace}", "{Ctrl+V}", "{Wait 100}"] {
                    if ui.small_button(tok).clicked() {
                        text.push_str(tok);
                    }
                }
                if ui
                    .small_button("Key…")
                    .on_hover_text("Press any key to insert it as {KEY}")
                    .clicked()
                {
                    *req = Some(Req::Bind(Bind::TypeInsert(i)));
                }
                if ui
                    .small_button("📍 Click…")
                    .on_hover_text("Hover over a spot and press any key to insert {Click X Y}")
                    .clicked()
                {
                    *req = Some(Req::Bind(Bind::TypeClick(i)));
                }
            });
            match typing::parse(text) {
                Err(e) => {
                    ui.colored_label(RED, format!("⚠ {e}"));
                }
                Ok(_) => {
                    let bad = typing::untypable(text, lay);
                    if !bad.is_empty() {
                        let b: String = bad.into_iter().collect();
                        ui.colored_label(
                            WARN,
                            format!("⚠ Layout '{}' can't type: {b}  (they'll be skipped)", lay.name),
                        );
                    }
                }
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Speed:");
                for (name, tip, c, h, s) in [
                    ("Instant", "Desktop apps and terminals", 1.0, 1.0, 10.0),
                    ("Fast", "Most apps and many games", 8.0, 8.0, 50.0),
                    (
                        "Game-safe",
                        "Games that read input once per frame (60 FPS or lower)",
                        20.0,
                        20.0,
                        120.0,
                    ),
                ] {
                    let on = *char_ms == c && *hold_ms == h && *settle_ms == s;
                    if ui.selectable_label(on, name).on_hover_text(tip).clicked() {
                        (*char_ms, *hold_ms, *settle_ms) = (c, h, s);
                    }
                }
                let per = (*char_ms + *hold_ms).max(0.01);
                ui.weak(format!("≈ {:.0} chars/s", 1000.0 / per));
            });
            egui::Grid::new("type")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Delay between keys");
                    ui.add(
                        egui::DragValue::new(char_ms)
                            .range(0.0..=10_000.0)
                            .speed(0.1)
                            .suffix(" ms")
                            .max_decimals(2),
                    );
                    ui.end_row();
                    ui.label("Key hold time")
                        .on_hover_text("Some apps/games drop keys held for 0 ms");
                    ui.add(
                        egui::DragValue::new(hold_ms)
                            .range(0.0..=1_000.0)
                            .speed(0.1)
                            .suffix(" ms")
                            .max_decimals(2),
                    );
                    ui.end_row();
                    ui.label("Pause before special keys").on_hover_text(
                        "Waits after typed text before {Enter}, {Tab}, combos, etc. so the app has \
                         processed every character first",
                    );
                    ui.add(
                        egui::DragValue::new(settle_ms)
                            .range(0.0..=10_000.0)
                            .speed(0.5)
                            .suffix(" ms")
                            .max_decimals(1),
                    );
                    ui.end_row();
                    ui.label("Repeat count");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(count).range(0..=1_000_000));
                        ui.weak("0 = until stopped");
                    });
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.weak(
                "Special keys go in braces: {Enter} {Esc} {F5} {Up} {Ctrl+Shift+T} {Alt+Tab}\n\
                 Repeat: {Tab 3}   Hold/release: {Shift down} … {Shift up}   Pause: {Wait 250}\n\
                 Literal braces: {{ and }}.  Accents and AltGr symbols follow your keyboard layout.",
            );
        }
        Action::Remap { to } => {
            egui::Grid::new("remap")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Acts as");
                    if chips(ui, to) {
                        *req = Some(Req::Bind(Bind::RemapTo(i)));
                    }
                    ui.end_row();
                });
            ui.weak("While the trigger is held, these keys are held. Add several for a combo (e.g. Ctrl + C).");
            if !exclusive {
                ui.colored_label(
                    WARN,
                    "⚠ Without Exclusive input the original key is still sent as well.",
                );
            }
        }
    }
}

impl App {
    fn open_bind(&mut self, b: Bind) {
        self.bind = Some(b);
        self.filter.clear();
        self.capture_left = false;
        *self.sh.capture.lock() = Capture::Waiting;
    }

    fn close_bind(&mut self) {
        self.bind = None;
        *self.sh.capture.lock() = Capture::Idle;
    }

    fn apply_bind(&mut self, b: Bind, code: Option<u16>) {
        let k = code.map(K);
        let push = |v: &mut Vec<K>| {
            if let Some(k) = k {
                if !v.contains(&k) {
                    v.push(k);
                }
            }
        };
        match b {
            Bind::ClickPos(..) | Bind::TypeClick(_) => {}
            Bind::Toggle => self.cfg.toggle_key = k,
            Bind::RecordStop => self.cfg.record_stop_key = k,
            Bind::Trigger(i) => {
                if let Some(m) = self.cfg.macros.get_mut(i) {
                    m.trigger = k;
                }
            }
            Bind::Modifier(i) => {
                if let Some(m) = self.cfg.macros.get_mut(i) {
                    push(&mut m.modifiers);
                }
            }
            Bind::RepeatKey(i) => {
                if let Some(Macro {
                    action: Action::Repeat { keys, .. },
                    ..
                }) = self.cfg.macros.get_mut(i)
                {
                    push(keys);
                }
            }
            Bind::RemapTo(i) => {
                if let Some(Macro {
                    action: Action::Remap { to },
                    ..
                }) = self.cfg.macros.get_mut(i)
                {
                    push(to);
                }
            }
            Bind::TypeInsert(i) => {
                if let (
                    Some(Macro {
                        action: Action::Type { text, .. },
                        ..
                    }),
                    Some(c),
                ) = (self.cfg.macros.get_mut(i), code)
                {
                    let n = keys::name(c);
                    text.push_str(&format!("{{{}}}", n.strip_prefix("KEY_").unwrap_or(&n)));
                }
            }
            Bind::StepKey(i, s) => {
                if let (
                    Some(Macro {
                        action: Action::Sequence { steps, .. },
                        ..
                    }),
                    Some(k),
                ) = (self.cfg.macros.get_mut(i), k)
                {
                    if let Some(Step::Tap { key, .. } | Step::Down { key } | Step::Up { key }) = steps.get_mut(s) {
                        *key = k
                    }
                }
            }
        }
    }

    fn apply_position(&mut self, b: Bind, px: i32, py: i32) {
        match b {
            Bind::ClickPos(i, s) => {
                if let Some(Macro {
                    action: Action::Sequence { steps, .. },
                    ..
                }) = self.cfg.macros.get_mut(i)
                {
                    if let Some(Step::Click { x, y, .. }) = steps.get_mut(s) {
                        (*x, *y) = (px, py);
                    }
                }
            }
            Bind::TypeClick(i) => {
                if let Some(Macro {
                    action: Action::Type { text, .. },
                    ..
                }) = self.cfg.macros.get_mut(i)
                {
                    text.push_str(&format!("{{Click {px} {py}}}"));
                }
            }
            _ => {}
        }
        self.sh.log(format!("Picked position {px}, {py}"));
    }

    fn poll_engine(&mut self) {
        if let Some(b) = self.bind {
            let cap = *self.sh.capture.lock();
            match cap {
                Capture::Got(_) if b.is_position() => {
                    self.close_bind();
                    match cursor::query() {
                        Ok(p) => self.apply_position(b, p.x.round() as i32, p.y.round() as i32),
                        Err(e) => self.sh.log(format!("Pick position failed: {e}")),
                    }
                }
                Capture::Got(c) => {
                    self.apply_bind(b, Some(c));
                    self.close_bind();
                }
                Capture::Cancelled => self.close_bind(),
                _ => {}
            }
        }
        let rec = self.sh.recorded.lock().take();
        if let (Some(new), Some(i)) = (rec, self.recording_for.take()) {
            if !new.is_empty() {
                if let Some(m) = self.cfg.macros.get_mut(i) {
                    match &mut m.action {
                        Action::Sequence { steps, .. } => *steps = new,
                        a => {
                            *a = Action::Sequence {
                                steps: new,
                                speed: 1.0,
                                count: 1,
                            }
                        }
                    }
                }
            }
        }
    }

    fn sync(&mut self) {
        if self.cfg != self.applied {
            self.sh.apply_config(self.cfg.clone());
            self.applied = self.cfg.clone();
            self.dirty = Some(Instant::now());
        }
        if self.dirty.is_some_and(|t| t.elapsed() > Duration::from_millis(600)) {
            self.save_err = config::save(&self.path, &self.cfg).err();
            self.dirty = None;
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let on = self.sh.enabled.load(Relaxed);
                let (txt, col) = if on {
                    ("●  ENABLED", GREEN)
                } else {
                    ("○  DISABLED", GREY)
                };
                if ui
                    .add(
                        egui::Button::new(RichText::new(txt).strong().color(Color32::WHITE))
                            .fill(col)
                            .min_size(egui::vec2(130.0, 28.0)),
                    )
                    .clicked()
                {
                    self.sh.set_enabled(!on);
                }
                ui.label("Toggle key");
                if key_btn(ui, self.cfg.toggle_key) {
                    self.open_bind(Bind::Toggle);
                }
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Macros, "Macros");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.selectable_value(&mut self.tab, Tab::Status, "Devices & Log");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(e) = &self.save_err {
                        ui.colored_label(RED, "Save failed").on_hover_text(e);
                    } else if self.dirty.is_some() {
                        ui.weak("saving…");
                    } else {
                        ui.weak("saved ✓");
                    }
                    if self.sh.is_recording() {
                        ui.colored_label(RED, "● REC");
                    }
                    if self
                        .undo
                        .as_ref()
                        .is_some_and(|(_, _, t)| t.elapsed() > Duration::from_secs(15))
                    {
                        self.undo = None;
                    }
                    if let Some((i, m, _)) = &self.undo {
                        if ui.button("↶ Undo").clicked() {
                            let i = (*i).min(self.cfg.macros.len());
                            self.cfg.macros.insert(i, m.clone());
                            self.sel = i;
                            self.undo = None;
                        } else {
                            ui.weak(format!("Deleted “{}”", m.name));
                        }
                    }
                });
            });
            ui.add_space(2.0);
        });
    }

    fn delete_macro(&mut self, i: usize) {
        if i < self.cfg.macros.len() {
            let m = self.cfg.macros.remove(i);
            self.undo = Some((i, m, Instant::now()));
            self.sel = self.sel.min(self.cfg.macros.len().saturating_sub(1));
        }
    }

    fn duplicate_macro(&mut self, i: usize) {
        if let Some(m) = self.cfg.macros.get(i) {
            let mut m = m.clone();
            m.name.push_str(" (copy)");
            self.cfg.macros.insert(i + 1, m);
            self.sel = i + 1;
        }
    }

    fn move_macro(&mut self, i: usize, up: bool) {
        let j = if up { i.checked_sub(1) } else { Some(i + 1) };
        if let Some(j) = j.filter(|&j| j < self.cfg.macros.len()) {
            self.cfg.macros.swap(i, j);
            self.sel = j;
        }
    }

    fn macro_list(&mut self, ctx: &egui::Context) {
        enum Op {
            Delete(usize),
            Duplicate(usize),
            Move(usize, bool),
        }
        let running = self.sh.running();
        let mut op = None;
        egui::SidePanel::left("list")
            .resizable(true)
            .default_width(270.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("Macros");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("＋ New").clicked() {
                            let n = self.cfg.macros.len() + 1;
                            self.cfg.macros.push(Macro {
                                name: format!("Macro {n}"),
                                ..Default::default()
                            });
                            self.sel = self.cfg.macros.len() - 1;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - 40.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let n = self.cfg.macros.len();
                        for (i, m) in self.cfg.macros.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut m.enabled, "").on_hover_text("Enabled");
                                let title = if running.contains(&i) {
                                    format!("▶ {}", m.name)
                                } else {
                                    m.name.clone()
                                };
                                let r =
                                    ui.add(egui::SelectableLabel::new(self.sel == i, RichText::new(title).strong()));
                                if r.clicked() {
                                    self.sel = i;
                                }
                                r.context_menu(|ui| {
                                    if ui.button("Duplicate").clicked() {
                                        op = Some(Op::Duplicate(i));
                                        ui.close_menu();
                                    }
                                    if ui.add_enabled(i > 0, egui::Button::new("Move up")).clicked() {
                                        op = Some(Op::Move(i, true));
                                        ui.close_menu();
                                    }
                                    if ui.add_enabled(i + 1 < n, egui::Button::new("Move down")).clicked() {
                                        op = Some(Op::Move(i, false));
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    if ui.button(RichText::new("🗑 Delete").color(RED)).clicked() {
                                        op = Some(Op::Delete(i));
                                        ui.close_menu();
                                    }
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("🗑").on_hover_text("Delete (can be undone)").clicked() {
                                        op = Some(Op::Delete(i));
                                    }
                                });
                            });
                            ui.weak(format!("      {}", summary(m)));
                            ui.add_space(2.0);
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    let n = self.cfg.macros.len();
                    let sel = self.sel;
                    let has = sel < n;
                    if ui.add_enabled(has, egui::Button::new("Duplicate")).clicked() {
                        op = Some(Op::Duplicate(sel));
                    }
                    if ui.add_enabled(has, egui::Button::new("Delete")).clicked() {
                        op = Some(Op::Delete(sel));
                    }
                    if ui.add_enabled(has && sel > 0, egui::Button::new("⬆")).clicked() {
                        op = Some(Op::Move(sel, true));
                    }
                    if ui.add_enabled(has && sel + 1 < n, egui::Button::new("⬇")).clicked() {
                        op = Some(Op::Move(sel, false));
                    }
                });
            });
        match op {
            Some(Op::Delete(i)) => self.delete_macro(i),
            Some(Op::Duplicate(i)) => self.duplicate_macro(i),
            Some(Op::Move(i, up)) => self.move_macro(i, up),
            None => {}
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(6.0);
        let mut bind = None;
        egui::Grid::new("settings").num_columns(2).spacing([20.0, 10.0]).show(ui, |ui| {
            ui.label("Enable/disable key");
            ui.horizontal(|ui| {
                if key_btn(ui, self.cfg.toggle_key) {
                    bind = Some(Bind::Toggle);
                }
                if ui.small_button("clear").clicked() {
                    self.cfg.toggle_key = None;
                }
            });
            ui.end_row();

            ui.label("Start enabled");
            ui.checkbox(&mut self.cfg.start_enabled, "Macros are active when evmacro starts");
            ui.end_row();

            ui.label("Exclusive input");
            ui.vertical(|ui| {
                ui.checkbox(&mut self.cfg.exclusive, "Grab keyboards/mice and forward through a virtual device");
                ui.weak("Needed to block trigger keys and for remaps. Adds only microseconds of latency.\nIf evmacro exits, devices are released automatically.");
            });
            ui.end_row();

            ui.label("Stop-recording key");
            ui.horizontal(|ui| {
                if key_btn(ui, self.cfg.record_stop_key) {
                    bind = Some(Bind::RecordStop);
                }
                if ui.small_button("clear").clicked() {
                    self.cfg.record_stop_key = None;
                }
            });
            ui.end_row();

            ui.label("Record mouse movement");
            ui.checkbox(&mut self.cfg.record_mouse_motion, "Include relative mouse motion in recordings");
            ui.end_row();

            ui.label("On/off feedback");
            ui.vertical(|ui| {
                ui.checkbox(&mut self.cfg.feedback_sound, "Sound (rising tone = on, falling = off)");
                ui.add_enabled(
                    self.cfg.feedback_sound,
                    egui::Slider::new(&mut self.cfg.feedback_volume, 0.0..=1.0).text("volume").show_value(false),
                );
                ui.checkbox(&mut self.cfg.feedback_notify, "Desktop notification");
                ui.horizontal(|ui| {
                    let (snd, ntf, vol) = (self.cfg.feedback_sound, self.cfg.feedback_notify, self.cfg.feedback_volume);
                    if ui.button("Test ON").clicked() {
                        crate::feedback::signal(true, snd, ntf, vol);
                    }
                    if ui.button("Test OFF").clicked() {
                        crate::feedback::signal(false, snd, ntf, vol);
                    }
                });
            });
            ui.end_row();

            ui.label("Keyboard layout");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cfg.keyboard_layout)
                            .hint_text(format!("auto: {}", typing::detect()))
                            .desired_width(160.0),
                    );
                    if !self.cfg.keyboard_layout.is_empty() && ui.small_button("auto").clicked() {
                        self.cfg.keyboard_layout.clear();
                    }
                });
                ui.weak("Used by “Type text” so symbols and accents come out right. Examples: latam, es, us, us(intl).");
            });
            ui.end_row();

            ui.label("Config file");
            ui.monospace(self.path.display().to_string());
            ui.end_row();
        });
        if let Some(b) = bind {
            self.open_bind(b);
        }
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        ui.heading("Devices");
        let devs = self.sh.devices.lock().clone();
        if devs.is_empty() {
            ui.colored_label(WARN, "No keyboards/mice readable. Run setup.sh and log out/in.");
        }
        egui::Grid::new("devs")
            .num_columns(4)
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for (p, d) in &devs {
                    ui.monospace(p.display().to_string());
                    ui.label(&d.name);
                    ui.weak(d.kind);
                    if d.grabbed {
                        ui.colored_label(GREEN, "grabbed");
                    } else {
                        ui.weak("shared");
                    }
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
        ui.heading("Log");
        let log: Vec<String> = self.sh.log.lock().iter().cloned().collect();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for l in log {
                    ui.monospace(l);
                }
            });
    }

    fn picker(&mut self, ctx: &egui::Context) {
        let Some(b) = self.bind else { return };
        let mut chosen: Option<Option<u16>> = None;
        let mut cancel = false;
        if b.is_position() {
            egui::Window::new("Pick a position")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    {
                        let mut cap = self.sh.capture.lock();
                        if *cap == Capture::Idle {
                            *cap = Capture::Waiting;
                        }
                    }
                    ui.label(
                        RichText::new("Hover over the spot, then press any key")
                            .size(17.0)
                            .strong(),
                    );
                    ui.weak("The cursor position is read when you press it. Esc cancels.");
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            if cancel {
                self.close_bind();
            }
            return;
        }
        egui::Window::new("Choose key")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(360.0)
            .show(ctx, |ui| {
                let listening = matches!(*self.sh.capture.lock(), Capture::Waiting | Capture::WaitingAny);
                ui.label(
                    RichText::new(if listening {
                        "Press any key or mouse button…"
                    } else {
                        "Paused while typing"
                    })
                    .size(17.0)
                    .strong(),
                );
                ui.weak("The next key or button you press is used. Esc cancels.");
                ui.checkbox(&mut self.capture_left, "Capture left click too")
                    .on_hover_text("After ticking this, your next left click anywhere is taken as the key");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("…or search keys")
                        .desired_width(f32::INFINITY),
                );
                {
                    let mut cap = self.sh.capture.lock();
                    if matches!(*cap, Capture::Idle | Capture::Waiting | Capture::WaitingAny) {
                        *cap = if r.has_focus() {
                            Capture::Idle
                        } else if self.capture_left {
                            Capture::WaitingAny
                        } else {
                            Capture::Waiting
                        };
                    }
                }
                let f = self.filter.to_ascii_lowercase();
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for (c, n) in keys::all() {
                        let p = keys::pretty(*c);
                        if !f.is_empty() && !p.to_ascii_lowercase().contains(&f) && !n.to_ascii_lowercase().contains(&f)
                        {
                            continue;
                        }
                        if ui.selectable_label(false, format!("{p}    ({n})")).clicked() {
                            chosen = Some(Some(*c));
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if b.clearable() && ui.button("None").clicked() {
                        chosen = Some(None);
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if let Some(c) = chosen {
            self.apply_bind(b, c);
            self.close_bind();
        } else if cancel {
            self.close_bind();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(50));
        self.poll_engine();
        self.top_bar(ctx);

        if self.tab == Tab::Macros {
            self.macro_list(ctx);
        }
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Macros => {
                let exclusive = self.cfg.exclusive;
                let recording = self.sh.is_recording();
                let sel = self.sel;
                let mut req = None;
                let mut delete = false;
                if self.layout_spec != self.cfg.keyboard_layout {
                    self.layout_spec = self.cfg.keyboard_layout.clone();
                    self.layout = typing::layout(&self.layout_spec);
                }
                let lay = self.layout.clone();
                let conflicts: Vec<String> = match self.cfg.macros.get(sel) {
                    Some(cur) if cur.trigger.is_some() => {
                        let mut mods = cur.modifiers.clone();
                        mods.sort_by_key(|k| k.0);
                        self.cfg
                            .macros
                            .iter()
                            .enumerate()
                            .filter(|(j, o)| {
                                let mut om = o.modifiers.clone();
                                om.sort_by_key(|k| k.0);
                                *j != sel && o.enabled && o.trigger == cur.trigger && om == mods
                            })
                            .map(|(_, o)| o.name.clone())
                            .collect()
                    }
                    _ => vec![],
                };
                if let Some(m) = self.cfg.macros.get_mut(sel) {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        macro_editor(
                            ui,
                            m,
                            sel,
                            exclusive,
                            recording,
                            &lay,
                            &conflicts,
                            &mut req,
                            &mut delete,
                        );
                    });
                } else {
                    ui.centered_and_justified(|ui| ui.weak("No macro selected — click ＋ New"));
                }
                match req {
                    Some(Req::Bind(b)) => self.open_bind(b),
                    Some(Req::RecStart) => {
                        self.recording_for = Some(sel);
                        self.sh.start_recording();
                    }
                    Some(Req::RecStop) => self.sh.stop_recording(true),
                    None => {}
                }
                if delete {
                    self.delete_macro(sel);
                }
            }
            Tab::Settings => self.settings(ui),
            Tab::Status => self.status(ui),
        });

        self.picker(ctx);
        self.sync();
    }
}
