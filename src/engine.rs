use crate::config::{Action, ClickMethod, Config, Macro, Mode, Step, K};
use crate::cursor;
use crate::keys;
use crate::typing::{self, Layout, Tok};
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{
    AbsInfo, AbsoluteAxisType, AttributeSet, Device, EventType, InputEvent, Key, PropType, RelativeAxisType,
    UinputAbsSetup,
};
use parking_lot::{Mutex, RwLock};
use rand::Rng;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::*};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub const VIRT_NAME: &str = "evmacro virtual input";

const KEY_ESC: u16 = 1;
const BTN_LEFT: u16 = 0x110;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capture {
    Idle,
    Waiting,
    WaitingAny,
    Got(u16),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct DevInfo {
    pub name: String,
    pub kind: &'static str,
    pub grabbed: bool,
}

struct Runner {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    sig: Macro,
}

pub struct Shared {
    pub config: RwLock<Arc<Config>>,
    pub enabled: AtomicBool,
    out: Mutex<VirtualDevice>,
    pressed: Mutex<HashSet<u16>>,
    captured: Mutex<HashSet<u16>>,
    blocked: Mutex<HashSet<u16>>,
    runners: Mutex<HashMap<usize, Runner>>,
    remaps: Mutex<HashMap<usize, (Macro, Vec<u16>)>>,
    pub capture: Mutex<Capture>,
    recording: AtomicBool,
    recorder: Mutex<Option<Recorder>>,
    pub recorded: Mutex<Option<Vec<Step>>>,
    pub devices: Mutex<BTreeMap<PathBuf, DevInfo>>,
    pub log: Mutex<VecDeque<String>>,
    t0: Instant,
    pointer: Mutex<Option<VirtualDevice>>,
    touch: Mutex<Option<VirtualDevice>>,
    touch_id: AtomicI32,
}

fn key_ev(code: u16, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY, code, value)
}
fn rel_ev(code: u16, value: i32) -> InputEvent {
    InputEvent::new(EventType::RELATIVE, code, value)
}
fn dur_ms(ms: f64) -> Duration {
    Duration::from_secs_f64(ms.max(0.0) / 1000.0)
}

const ABS_MAX: i32 = 32767;
const POINTER_NAME: &str = "evmacro virtual pointer";
const TOUCH_NAME: &str = "evmacro virtual touch";

fn abs_axis(code: AbsoluteAxisType, max: i32, res: i32) -> UinputAbsSetup {
    UinputAbsSetup::new(code, AbsInfo::new(0, 0, max, 0, 0, res))
}

fn abs_ev(code: AbsoluteAxisType, value: i32) -> InputEvent {
    InputEvent::new(EventType::ABSOLUTE, code.0, value)
}

fn build_pointer() -> std::io::Result<VirtualDevice> {
    let mut keyset = AttributeSet::<Key>::new();
    for k in [Key::BTN_LEFT, Key::BTN_RIGHT, Key::BTN_MIDDLE] {
        keyset.insert(k);
    }
    VirtualDeviceBuilder::new()?
        .name(POINTER_NAME)
        .with_keys(&keyset)?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_X, ABS_MAX, 0))?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_Y, ABS_MAX, 0))?
        .build()
}

fn build_touch() -> std::io::Result<VirtualDevice> {
    let mut keyset = AttributeSet::<Key>::new();
    keyset.insert(Key::BTN_TOUCH);
    let mut props = AttributeSet::<PropType>::new();
    props.insert(PropType::DIRECT);
    VirtualDeviceBuilder::new()?
        .name(TOUCH_NAME)
        .with_keys(&keyset)?
        .with_properties(&props)?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_X, ABS_MAX, 100))?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_Y, ABS_MAX, 100))?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_MT_SLOT, 9, 0))?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_MT_TRACKING_ID, 65535, 0))?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_MT_POSITION_X, ABS_MAX, 100))?
        .with_absolute_axis(&abs_axis(AbsoluteAxisType::ABS_MT_POSITION_Y, ABS_MAX, 100))?
        .build()
}

fn to_abs(x: i32, y: i32, g: &cursor::Info) -> (i32, i32) {
    let conv = |v: f64, origin: f64, size: f64| {
        let a = ((v - origin + 0.5) * (ABS_MAX as f64 + 1.0) / size.max(1.0)).floor();
        a.clamp(0.0, ABS_MAX as f64) as i32
    };
    (conv(x as f64, g.sx, g.sw), conv(y as f64, g.sy, g.sh))
}

fn config_click_methods(cfg: &Config) -> (bool, bool) {
    let (mut pointer, mut touch) = (false, false);
    let mut note = |m: ClickMethod| match m {
        ClickMethod::Pointer => pointer = true,
        ClickMethod::Touch => touch = true,
    };
    for m in cfg.macros.iter().filter(|m| m.enabled) {
        let texts: Vec<&str> = match &m.action {
            Action::Type { text, .. } => vec![text.as_str()],
            Action::Sequence { steps, .. } => steps
                .iter()
                .filter_map(|s| match s {
                    Step::Type { text, .. } => Some(text.as_str()),
                    Step::Click { method, .. } => {
                        note(*method);
                        None
                    }
                    _ => None,
                })
                .collect(),
            _ => vec![],
        };
        for t in texts {
            for tok in typing::parse(t).unwrap_or_default() {
                if let Tok::Click { method, .. } = tok {
                    note(method);
                }
            }
        }
    }
    (pointer, touch)
}

fn build_virtual() -> std::io::Result<VirtualDevice> {
    let mut keyset = AttributeSet::<Key>::new();
    for &(c, _) in keys::all() {
        if (0x100..=0x10f).contains(&c) || (0x118..=0x15f).contains(&c) || c >= 0x2c0 {
            continue;
        }
        keyset.insert(Key::new(c));
    }
    let mut rel = AttributeSet::<RelativeAxisType>::new();
    for r in [
        RelativeAxisType::REL_X,
        RelativeAxisType::REL_Y,
        RelativeAxisType::REL_WHEEL,
        RelativeAxisType::REL_HWHEEL,
        RelativeAxisType::REL_WHEEL_HI_RES,
        RelativeAxisType::REL_HWHEEL_HI_RES,
    ] {
        rel.insert(r);
    }
    VirtualDeviceBuilder::new()?
        .name(VIRT_NAME)
        .with_keys(&keyset)?
        .with_relative_axes(&rel)?
        .build()
}

impl Shared {
    pub fn start(cfg: Config) -> std::io::Result<Arc<Shared>> {
        let out = build_virtual()?;
        let sh = Arc::new(Shared {
            enabled: AtomicBool::new(cfg.start_enabled),
            config: RwLock::new(Arc::new(cfg)),
            out: Mutex::new(out),
            pressed: Mutex::default(),
            captured: Mutex::default(),
            blocked: Mutex::default(),
            runners: Mutex::default(),
            remaps: Mutex::default(),
            capture: Mutex::new(Capture::Idle),
            recording: AtomicBool::new(false),
            recorder: Mutex::new(None),
            recorded: Mutex::new(None),
            devices: Mutex::default(),
            log: Mutex::default(),
            t0: Instant::now(),
            pointer: Mutex::new(None),
            touch: Mutex::new(None),
            touch_id: AtomicI32::new(1),
        });
        let s2 = sh.clone();
        thread::Builder::new()
            .name("scanner".into())
            .spawn(move || scanner(s2))?;
        let spec = sh.config.read().keyboard_layout.clone();
        thread::spawn(move || {
            typing::layout(&spec);
        });
        sh.prewarm_click_devices();
        sh.log(format!(
            "Engine started ({})",
            if sh.enabled.load(Relaxed) {
                "enabled"
            } else {
                "disabled"
            }
        ));
        Ok(sh)
    }

    pub fn log(&self, msg: impl Into<String>) {
        let m = msg.into();
        eprintln!("[evmacro] {m}");
        let mut l = self.log.lock();
        if l.len() >= 400 {
            l.pop_front();
        }
        l.push_back(format!("{:>9.2}s  {m}", self.t0.elapsed().as_secs_f64()));
    }

    pub fn emit(&self, evs: &[InputEvent]) {
        if evs.is_empty() {
            return;
        }
        if let Err(e) = self.out.lock().emit(evs) {
            self.log(format!("uinput write failed: {e}"));
        }
    }

    fn ensure_device(
        &self,
        slot: &Mutex<Option<VirtualDevice>>,
        build: fn() -> std::io::Result<VirtualDevice>,
        what: &str,
    ) -> bool {
        let mut d = slot.lock();
        if d.is_some() {
            return true;
        }
        match build() {
            Ok(v) => {
                *d = Some(v);
                drop(d);
                self.log(format!("Created virtual {what}"));
                thread::sleep(Duration::from_millis(300));
                true
            }
            Err(e) => {
                self.log(format!("Could not create virtual {what}: {e}"));
                false
            }
        }
    }

    fn emit_on(&self, slot: &Mutex<Option<VirtualDevice>>, evs: &[InputEvent]) {
        if let Some(d) = slot.lock().as_mut() {
            if let Err(e) = d.emit(evs) {
                self.log(format!("uinput write failed: {e}"));
            }
        }
    }

    fn prewarm_click_devices(self: &Arc<Self>) {
        let (p, t) = config_click_methods(&self.config.read());
        if !p && !t {
            return;
        }
        let sh = self.clone();
        thread::spawn(move || {
            if p {
                sh.ensure_device(&sh.pointer, build_pointer, "pointer");
            }
            if t {
                sh.ensure_device(&sh.touch, build_touch, "touchscreen");
            }
            let _ = cursor::geometry();
        });
    }

    fn move_pointer(&self, ax: i32, ay: i32) {
        let nudge = |v: i32| if v < ABS_MAX { v + 1 } else { v - 1 };
        self.emit_on(
            &self.pointer,
            &[
                abs_ev(AbsoluteAxisType::ABS_X, nudge(ax)),
                abs_ev(AbsoluteAxisType::ABS_Y, nudge(ay)),
                abs_ev(AbsoluteAxisType::ABS_X, ax),
                abs_ev(AbsoluteAxisType::ABS_Y, ay),
            ],
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn click_at(
        &self,
        x: i32,
        y: i32,
        button: u16,
        method: ClickMethod,
        back: bool,
        hold: Duration,
        stop: &AtomicBool,
    ) -> bool {
        let pause = |d: Duration| wait_until(Instant::now() + d, stop);
        match method {
            ClickMethod::Touch => {
                let g = match cursor::geometry() {
                    Ok(g) => g,
                    Err(e) => {
                        self.log(e);
                        return true;
                    }
                };
                if !self.ensure_device(&self.touch, build_touch, "touchscreen") {
                    return true;
                }
                let (ax, ay) = to_abs(x, y, &g);
                let id = self.touch_id.fetch_add(1, Relaxed) & 0xffff;
                self.emit_on(
                    &self.touch,
                    &[
                        abs_ev(AbsoluteAxisType::ABS_MT_SLOT, 0),
                        abs_ev(AbsoluteAxisType::ABS_MT_TRACKING_ID, id),
                        abs_ev(AbsoluteAxisType::ABS_MT_POSITION_X, ax),
                        abs_ev(AbsoluteAxisType::ABS_MT_POSITION_Y, ay),
                        abs_ev(AbsoluteAxisType::ABS_X, ax),
                        abs_ev(AbsoluteAxisType::ABS_Y, ay),
                        key_ev(Key::BTN_TOUCH.code(), 1),
                    ],
                );
                let ok = pause(hold.max(Duration::from_millis(20)));
                self.emit_on(
                    &self.touch,
                    &[
                        abs_ev(AbsoluteAxisType::ABS_MT_TRACKING_ID, -1),
                        key_ev(Key::BTN_TOUCH.code(), 0),
                    ],
                );
                ok
            }
            ClickMethod::Pointer => {
                let info = if back { cursor::query() } else { cursor::geometry() };
                let g = match info {
                    Ok(g) => g,
                    Err(e) => {
                        self.log(e);
                        return true;
                    }
                };
                if !self.ensure_device(&self.pointer, build_pointer, "pointer") {
                    return true;
                }
                let (ax, ay) = to_abs(x, y, &g);
                self.move_pointer(ax, ay);
                thread::sleep(Duration::from_millis(12));
                self.emit_on(&self.pointer, &[key_ev(button, 1)]);
                let ok = pause(hold.max(Duration::from_millis(10)));
                self.emit_on(&self.pointer, &[key_ev(button, 0)]);
                if back {
                    thread::sleep(Duration::from_millis(12));
                    let (bx, by) = to_abs(g.x.round() as i32, g.y.round() as i32, &g);
                    self.move_pointer(bx, by);
                }
                ok
            }
        }
    }

    fn press_keys(&self, ks: &[u16]) {
        let evs: Vec<_> = ks.iter().map(|&k| key_ev(k, 1)).collect();
        self.emit(&evs);
    }

    fn release_keys(&self, ks: &[u16]) {
        let evs: Vec<_> = ks.iter().rev().map(|&k| key_ev(k, 0)).collect();
        self.emit(&evs);
    }

    pub fn set_enabled(&self, v: bool) {
        self.enabled.store(v, Relaxed);
        if !v {
            self.stop_all();
        }
        self.log(if v { "Macros ENABLED" } else { "Macros DISABLED" });
        let c = self.config.read();
        crate::feedback::signal(v, c.feedback_sound, c.feedback_notify, c.feedback_volume);
    }

    fn stop_all(&self) {
        for (_, r) in self.runners.lock().drain() {
            r.stop.store(true, Relaxed);
        }
        let held: Vec<_> = self.remaps.lock().drain().map(|(_, (_, k))| k).collect();
        for k in held {
            self.release_keys(&k);
        }
    }

    pub fn apply_config(self: &Arc<Self>, new: Config) {
        let new = Arc::new(new);
        *self.config.write() = new.clone();
        self.runners.lock().retain(|i, r| {
            let same = new.macros.get(*i) == Some(&r.sig);
            if !same {
                r.stop.store(true, Relaxed);
            }
            same
        });
        let mut release = Vec::new();
        self.remaps.lock().retain(|i, (sig, k)| {
            let same = new.macros.get(*i) == Some(&*sig);
            if !same {
                release.push(k.clone());
            }
            same
        });
        for k in release {
            self.release_keys(&k);
        }
        self.prewarm_click_devices();
    }

    pub fn running(&self) -> HashSet<usize> {
        self.runners
            .lock()
            .iter()
            .filter(|(_, r)| !r.done.load(Acquire))
            .map(|(i, _)| *i)
            .collect()
    }

    fn is_running(&self, i: usize) -> bool {
        self.runners.lock().get(&i).is_some_and(|r| !r.done.load(Acquire))
    }

    fn stop_runner(&self, i: usize) {
        if let Some(r) = self.runners.lock().remove(&i) {
            r.stop.store(true, Relaxed);
        }
    }

    fn release_physical_mods(&self) {
        let ks: Vec<u16> = {
            let p = self.pressed.lock();
            typing::MODIFIERS.iter().copied().filter(|k| p.contains(k)).collect()
        };
        self.release_keys(&ks);
    }

    fn effective_mode(m: &Macro) -> Mode {
        match m.action {
            Action::Type { .. } => Mode::Toggle,
            _ => m.mode,
        }
    }

    fn mods_ok(&self, m: &Macro) -> bool {
        let p = self.pressed.lock();
        m.modifiers.iter().all(|k| p.contains(&k.0))
    }

    fn spawn_runner(self: &Arc<Self>, idx: usize, m: &Macro) {
        let mut rs = self.runners.lock();
        if rs.get(&idx).is_some_and(|r| !r.done.load(Acquire)) {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        rs.insert(
            idx,
            Runner {
                stop: stop.clone(),
                done: done.clone(),
                sig: m.clone(),
            },
        );
        drop(rs);
        let sh = self.clone();
        let action = m.action.clone();
        let lay_spec = self.config.read().keyboard_layout.clone();
        let res = thread::Builder::new().name(format!("macro-{idx}")).spawn(move || {
            unsafe {
                libc::prctl(libc::PR_SET_TIMERSLACK, 1 as libc::c_ulong);
            }
            let _done = DoneGuard(done);
            let mut out = Out {
                sh: &sh,
                held: HashSet::new(),
            };
            run_action(&mut out, &action, &stop, &lay_spec);
        });
        if let Err(e) = res {
            self.log(format!("failed to start macro thread: {e}"));
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Relaxed)
    }

    pub fn start_recording(&self) {
        let mouse = self.config.read().record_mouse_motion;
        *self.recorder.lock() = Some(Recorder::new(mouse));
        self.recording.store(true, Relaxed);
        self.log("Recording started");
    }

    pub fn stop_recording(&self, trim_click: bool) {
        self.recording.store(false, Relaxed);
        if let Some(r) = self.recorder.lock().take() {
            let steps = r.finish(trim_click);
            self.log(format!("Recording stopped: {} steps", steps.len()));
            *self.recorded.lock() = Some(steps);
        }
    }

    fn on_key(self: &Arc<Self>, code: u16, value: i32) -> bool {
        if value == 1 {
            let mut cap = self.capture.lock();
            if (*cap == Capture::Waiting && code != BTN_LEFT) || *cap == Capture::WaitingAny {
                *cap = if code == KEY_ESC {
                    Capture::Cancelled
                } else {
                    Capture::Got(code)
                };
                drop(cap);
                self.captured.lock().insert(code);
                return false;
            }
        }
        {
            let mut c = self.captured.lock();
            if value == 1 {
                c.remove(&code);
            } else if c.contains(&code) {
                if value == 0 {
                    c.remove(&code);
                }
                return false;
            }
        }

        let cfg = self.config.read().clone();

        if self.recording.load(Relaxed) && cfg.toggle_key != Some(K(code)) {
            if cfg.record_stop_key == Some(K(code)) {
                self.stop_recording(false);
                self.captured.lock().insert(code);
                return false;
            }
            if value != 2 {
                if let Some(r) = self.recorder.lock().as_mut() {
                    r.key(code, value == 1);
                }
            }
        }

        match value {
            1 => {
                self.pressed.lock().insert(code);
            }
            0 => {
                self.pressed.lock().remove(&code);
            }
            _ => {}
        }

        if cfg.toggle_key == Some(K(code)) {
            self.set_enabled(!self.enabled.load(Relaxed));
            self.captured.lock().insert(code);
            return false;
        }

        let was_blocked = {
            let mut b = self.blocked.lock();
            match value {
                0 => b.remove(&code),
                2 => b.contains(&code),
                _ => {
                    b.remove(&code);
                    false
                }
            }
        };

        if value == 0 {
            for (i, m) in cfg.macros.iter().enumerate() {
                if m.trigger != Some(K(code)) {
                    continue;
                }
                match &m.action {
                    Action::Remap { .. } => {
                        let held = self.remaps.lock().remove(&i);
                        if let Some((_, k)) = held {
                            self.release_keys(&k);
                        }
                    }
                    _ if Self::effective_mode(m) == Mode::Hold => self.stop_runner(i),
                    _ => {}
                }
            }
            return !was_blocked;
        }

        if value == 2 || !self.enabled.load(Relaxed) {
            return !was_blocked;
        }

        let candidates: Vec<(usize, &Macro)> = cfg
            .macros
            .iter()
            .enumerate()
            .filter(|(_, m)| m.enabled && m.trigger == Some(K(code)))
            .collect();
        let best = candidates
            .iter()
            .filter(|(_, m)| self.mods_ok(m))
            .map(|(_, m)| m.modifiers.len())
            .max();
        let mut block = false;
        for (i, m) in candidates {
            let chosen = best == Some(m.modifiers.len()) && self.mods_ok(m);
            if let Action::Remap { to } = &m.action {
                if chosen && !to.is_empty() {
                    let k: Vec<u16> = to.iter().map(|k| k.0).collect();
                    self.press_keys(&k);
                    self.remaps.lock().insert(i, (m.clone(), k));
                    block = true;
                }
                continue;
            }
            match Self::effective_mode(m) {
                Mode::Hold => {
                    if chosen {
                        self.spawn_runner(i, m);
                        block |= m.block_trigger;
                    }
                }
                Mode::Toggle => {
                    if self.is_running(i) {
                        self.stop_runner(i);
                        block |= m.block_trigger;
                    } else if chosen {
                        self.spawn_runner(i, m);
                        block |= m.block_trigger;
                    }
                }
            }
        }
        if block {
            self.blocked.lock().insert(code);
            return false;
        }
        true
    }

    fn on_rel(&self, code: u16, value: i32) {
        if self.recording.load(Relaxed) {
            if let Some(r) = self.recorder.lock().as_mut() {
                r.rel(code, value);
            }
        }
    }
}

struct DoneGuard(Arc<AtomicBool>);
impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, Release);
    }
}

struct Out<'a> {
    sh: &'a Shared,
    held: HashSet<u16>,
}

impl Out<'_> {
    fn down(&mut self, ks: &[u16]) {
        self.held.extend(ks.iter().copied());
        self.sh.press_keys(ks);
    }
    fn up(&mut self, ks: &[u16]) {
        for k in ks {
            self.held.remove(k);
        }
        self.sh.release_keys(ks);
    }
}

impl Drop for Out<'_> {
    fn drop(&mut self) {
        let ks: Vec<u16> = self.held.drain().collect();
        self.sh.release_keys(&ks);
    }
}

fn wait_until(deadline: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        let rem = deadline - now;
        if rem > Duration::from_micros(300) {
            thread::sleep((rem - Duration::from_micros(200)).min(Duration::from_millis(5)));
        } else {
            std::hint::spin_loop();
        }
    }
}

#[derive(Clone, Copy)]
struct Timing {
    gap: Duration,
    hold: Duration,
    settle: Duration,
}

impl Timing {
    fn new(char_ms: f64, hold_ms: f64, settle_ms: f64) -> Self {
        Timing {
            gap: dur_ms(char_ms),
            hold: dur_ms(hold_ms),
            settle: dur_ms(settle_ms),
        }
    }
}

fn type_tokens(o: &mut Out, toks: &[Tok], lay: &Layout, tm: Timing, stop: &AtomicBool) -> bool {
    let Timing { gap, hold, settle } = tm;
    let pause = |d: Duration| d.is_zero() || wait_until(Instant::now() + d, stop);
    let mut missing: Vec<char> = Vec::new();
    let mut after_text = false;
    for t in toks {
        if stop.load(Relaxed) {
            return false;
        }
        if after_text && matches!(t, Tok::Combo { .. } | Tok::Down(_) | Tok::Click { .. }) {
            after_text = false;
            if !pause(settle) {
                return false;
            }
        }
        match t {
            Tok::Char(c) => match lay.strokes(*c) {
                Some(strokes) => {
                    after_text = true;
                    for s in strokes {
                        let mut mods = Vec::with_capacity(2);
                        if s.shift {
                            mods.push(typing::KEY_SHIFT);
                        }
                        if s.altgr {
                            mods.push(typing::KEY_ALTGR);
                        }
                        o.down(&mods);
                        o.down(&[s.key]);
                        if !pause(hold) {
                            return false;
                        }
                        o.up(&[s.key]);
                        o.up(&mods);
                        if !pause(gap) {
                            return false;
                        }
                    }
                }
                None => {
                    if !missing.contains(c) {
                        missing.push(*c);
                    }
                }
            },
            Tok::Combo { mods, key, times } => {
                for _ in 0..*times {
                    o.down(mods);
                    o.down(&[*key]);
                    if !pause(hold) {
                        return false;
                    }
                    o.up(&[*key]);
                    o.up(mods);
                    if !pause(gap) {
                        return false;
                    }
                }
            }
            Tok::Down(k) => o.down(&[*k]),
            Tok::Up(k) => o.up(&[*k]),
            Tok::Wait(ms) => {
                after_text = false;
                if !pause(dur_ms(*ms)) {
                    return false;
                }
            }
            Tok::Click {
                x,
                y,
                button,
                method,
                back,
            } => {
                if !o.sh.click_at(*x, *y, *button, *method, *back, hold, stop) || !pause(gap) {
                    return false;
                }
            }
        }
    }
    if !missing.is_empty() {
        let s: String = missing.into_iter().collect();
        o.sh.log(format!("Skipped characters not on layout '{}': {s}", lay.name));
    }
    true
}

fn run_action(o: &mut Out, action: &Action, stop: &AtomicBool, lay_spec: &str) {
    match action {
        Action::Repeat {
            keys,
            interval_ms,
            jitter_ms,
            press_ms,
            count,
        } => {
            if keys.is_empty() {
                return;
            }
            let ks: Vec<u16> = keys.iter().map(|k| k.0).collect();
            let interval = dur_ms(interval_ms.max(0.05));
            let press = dur_ms(press_ms.clamp(0.0, interval_ms.max(0.05)));
            let jitter = jitter_ms.max(0.0);
            let mut rng = rand::thread_rng();
            let mut base = Instant::now();
            let mut start = base;
            let mut n = 0u32;
            loop {
                if stop.load(Relaxed) {
                    return;
                }
                o.down(&ks);
                if !press.is_zero() && !wait_until(start + press, stop) {
                    return;
                }
                o.up(&ks);
                n += 1;
                if *count > 0 && n >= *count {
                    return;
                }
                base += interval;
                let now = Instant::now();
                if base < now {
                    base = now;
                }
                start = if jitter > 0.0 {
                    let off = rng.gen_range(-jitter..=jitter);
                    if off >= 0.0 {
                        base + dur_ms(off)
                    } else {
                        base.checked_sub(dur_ms(-off)).unwrap_or(base)
                    }
                } else {
                    base
                };
                if !wait_until(start, stop) {
                    return;
                }
            }
        }
        Action::Sequence { steps, speed, count } => {
            if steps.is_empty() {
                return;
            }
            let speed = if *speed > 0.0 { *speed } else { 1.0 };
            let timed = steps.iter().any(|s| match s {
                Step::Wait { ms } => *ms > 0.0,
                Step::Tap { hold_ms, .. } => *hold_ms > 0.0,
                Step::Type { .. } | Step::Click { .. } => true,
                _ => false,
            });
            let lay = steps
                .iter()
                .any(|s| matches!(s, Step::Type { .. }))
                .then(|| typing::layout(lay_spec));
            if lay.is_some() {
                o.sh.release_physical_mods();
            }
            let mut n = 0u32;
            loop {
                let mut t = Instant::now();
                for s in steps {
                    if stop.load(Relaxed) {
                        return;
                    }
                    match s {
                        Step::Down { key } => o.down(&[key.0]),
                        Step::Up { key } => o.up(&[key.0]),
                        Step::Tap { key, hold_ms } => {
                            o.down(&[key.0]);
                            t += dur_ms(hold_ms / speed);
                            if !wait_until(t, stop) {
                                return;
                            }
                            o.up(&[key.0]);
                        }
                        Step::Wait { ms } => {
                            t += dur_ms(ms / speed);
                            if !wait_until(t, stop) {
                                return;
                            }
                        }
                        Step::Move { dx, dy } => {
                            let mut e = Vec::with_capacity(2);
                            if *dx != 0 {
                                e.push(rel_ev(REL_X, *dx));
                            }
                            if *dy != 0 {
                                e.push(rel_ev(REL_Y, *dy));
                            }
                            o.sh.emit(&e);
                        }
                        Step::Wheel { amount } => {
                            o.sh.emit(&[rel_ev(REL_WHEEL, *amount), rel_ev(REL_WHEEL_HI_RES, amount * 120)])
                        }
                        Step::HWheel { amount } => {
                            o.sh.emit(&[rel_ev(REL_HWHEEL, *amount), rel_ev(REL_HWHEEL_HI_RES, amount * 120)])
                        }
                        Step::Click {
                            x,
                            y,
                            button,
                            method,
                            back,
                        } => {
                            if !o
                                .sh
                                .click_at(*x, *y, button.0, *method, *back, Duration::from_millis(10), stop)
                            {
                                return;
                            }
                            t = Instant::now();
                        }
                        Step::Type { text, char_ms } => {
                            let (Ok(toks), Some(lay)) = (typing::parse(text), lay.as_ref()) else {
                                o.sh.log(format!("Sequence: invalid text step '{text}'"));
                                continue;
                            };
                            if !type_tokens(
                                o,
                                &toks,
                                lay,
                                Timing::new(char_ms / speed, char_ms / speed, 50.0 / speed),
                                stop,
                            ) {
                                return;
                            }
                            t = Instant::now();
                        }
                    }
                }
                n += 1;
                if *count > 0 && n >= *count {
                    return;
                }
                if !timed && !wait_until(Instant::now() + Duration::from_millis(1), stop) {
                    return;
                }
            }
        }
        Action::Type {
            text,
            char_ms,
            hold_ms,
            settle_ms,
            count,
        } => {
            let toks = match typing::parse(text) {
                Ok(t) if !t.is_empty() => t,
                Ok(_) => return,
                Err(e) => {
                    o.sh.log(format!("Type macro: {e}"));
                    return;
                }
            };
            let lay = typing::layout(lay_spec);
            o.sh.release_physical_mods();
            let mut n = 0u32;
            loop {
                if !type_tokens(o, &toks, &lay, Timing::new(*char_ms, *hold_ms, *settle_ms), stop) {
                    return;
                }
                n += 1;
                if *count > 0 && n >= *count {
                    return;
                }
                if !wait_until(Instant::now() + Duration::from_millis(1), stop) {
                    return;
                }
            }
        }
        Action::Remap { .. } => {}
    }
}

struct Recorder {
    last: Instant,
    steps: Vec<Step>,
    mouse: bool,
    acc: (i32, i32),
}

impl Recorder {
    fn new(mouse: bool) -> Self {
        Recorder {
            last: Instant::now(),
            steps: vec![],
            mouse,
            acc: (0, 0),
        }
    }
    fn gap(&mut self) {
        let now = Instant::now();
        let ms = (now - self.last).as_secs_f64() * 1000.0;
        self.last = now;
        if ms >= 0.5 {
            self.steps.push(Step::Wait {
                ms: (ms * 10.0).round() / 10.0,
            });
        }
    }
    fn flush(&mut self) {
        if self.acc != (0, 0) {
            self.gap();
            self.steps.push(Step::Move {
                dx: self.acc.0,
                dy: self.acc.1,
            });
            self.acc = (0, 0);
        }
    }
    fn key(&mut self, code: u16, down: bool) {
        self.flush();
        self.gap();
        let key = K(code);
        self.steps
            .push(if down { Step::Down { key } } else { Step::Up { key } });
    }
    fn rel(&mut self, code: u16, v: i32) {
        match code {
            REL_X if self.mouse => self.acc.0 += v,
            REL_Y if self.mouse => self.acc.1 += v,
            REL_WHEEL => {
                self.flush();
                self.gap();
                self.steps.push(Step::Wheel { amount: v });
            }
            REL_HWHEEL => {
                self.flush();
                self.gap();
                self.steps.push(Step::HWheel { amount: v });
            }
            _ => {}
        }
        if self.acc != (0, 0) && self.last.elapsed() >= Duration::from_millis(8) {
            self.flush();
        }
    }
    fn finish(mut self, trim_click: bool) -> Vec<Step> {
        self.flush();
        let mut s = self.steps;
        if trim_click {
            if let Some(p) = s
                .iter()
                .rposition(|x| matches!(x, Step::Down { key } if key.0 == BTN_LEFT))
            {
                s.truncate(p);
            }
            while matches!(s.last(), Some(Step::Move { .. }) | Some(Step::Wait { .. })) {
                s.pop();
            }
        }
        let mut down = HashSet::new();
        let mut out = Vec::with_capacity(s.len());
        for st in s {
            match &st {
                Step::Down { key } => {
                    down.insert(key.0);
                    out.push(st);
                }
                Step::Up { key } => {
                    if down.remove(&key.0) {
                        out.push(st);
                    }
                }
                _ => out.push(st),
            }
        }
        while matches!(out.first(), Some(Step::Wait { .. })) {
            out.remove(0);
        }
        while matches!(out.last(), Some(Step::Wait { .. })) {
            out.pop();
        }
        for k in down {
            out.push(Step::Up { key: K(k) });
        }
        let mut c = Vec::with_capacity(out.len());
        let mut i = 0;
        while i < out.len() {
            if let Step::Down { key } = out[i] {
                match (out.get(i + 1), out.get(i + 2)) {
                    (Some(Step::Wait { ms }), Some(Step::Up { key: k2 })) if *k2 == key => {
                        c.push(Step::Tap { key, hold_ms: *ms });
                        i += 3;
                        continue;
                    }
                    (Some(Step::Up { key: k2 }), _) if *k2 == key => {
                        c.push(Step::Tap { key, hold_ms: 0.0 });
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            c.push(out[i].clone());
            i += 1;
        }
        c
    }
}

fn classify(d: &Device) -> Option<&'static str> {
    let keys = d.supported_keys();
    let kbd = keys.is_some_and(|k| k.contains(Key::KEY_A) && k.contains(Key::KEY_SPACE));
    let mouse = keys.is_some_and(|k| k.contains(Key::BTN_LEFT))
        && d.supported_relative_axes()
            .is_some_and(|r| r.contains(RelativeAxisType::REL_X));
    match (kbd, mouse) {
        (true, true) => Some("keyboard+mouse"),
        (true, false) => Some("keyboard"),
        (false, true) => Some("mouse"),
        _ => None,
    }
}

fn scanner(sh: Arc<Shared>) {
    let mut ignored: HashSet<PathBuf> = HashSet::new();
    let mut warned_perm = false;
    let mut tick = 0u32;
    loop {
        if tick.is_multiple_of(15) {
            ignored.clear();
        }
        tick = tick.wrapping_add(1);
        if let Ok(rd) = std::fs::read_dir("/dev/input") {
            for e in rd.flatten() {
                let p = e.path();
                let is_ev = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("event"));
                if !is_ev || ignored.contains(&p) || sh.devices.lock().contains_key(&p) {
                    continue;
                }
                let sys = Path::new("/sys/class/input")
                    .join(p.file_name().unwrap())
                    .join("device/name");
                if std::fs::read_to_string(sys).is_ok_and(|n| n.trim().starts_with("evmacro")) {
                    ignored.insert(p);
                    continue;
                }
                match Device::open(&p) {
                    Ok(d) => {
                        let name = d.name().unwrap_or("?").to_string();
                        let Some(kind) = (if name.starts_with("evmacro") {
                            None
                        } else {
                            classify(&d)
                        }) else {
                            ignored.insert(p);
                            continue;
                        };
                        sh.devices.lock().insert(
                            p.clone(),
                            DevInfo {
                                name: name.clone(),
                                kind,
                                grabbed: false,
                            },
                        );
                        sh.log(format!("Device added: {name} ({kind}) {}", p.display()));
                        let s2 = sh.clone();
                        let p2 = p.clone();
                        let r = thread::Builder::new()
                            .name(format!("dev-{name}"))
                            .spawn(move || device_loop(s2, p2, d));
                        if r.is_err() {
                            sh.devices.lock().remove(&p);
                        }
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::PermissionDenied && !warned_perm {
                            warned_perm = true;
                            sh.log(format!(
                                "No permission to read {} - run setup.sh (input group) and re-login",
                                p.display()
                            ));
                        }
                        ignored.insert(p);
                    }
                }
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
}

struct DevGuard {
    sh: Arc<Shared>,
    path: PathBuf,
}
impl Drop for DevGuard {
    fn drop(&mut self) {
        self.sh.devices.lock().remove(&self.path);
    }
}

fn set_grabbed(sh: &Shared, path: &Path, g: bool) {
    if let Some(d) = sh.devices.lock().get_mut(path) {
        d.grabbed = g;
    }
}

fn device_loop(sh: Arc<Shared>, path: PathBuf, mut dev: Device) {
    let _guard = DevGuard {
        sh: sh.clone(),
        path: path.clone(),
    };
    let fd = dev.as_raw_fd();
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    let mut grabbed = false;
    let mut grab_failed = false;
    let mut batch: Vec<InputEvent> = Vec::with_capacity(16);
    let mut down: HashSet<u16> = HashSet::new();

    loop {
        let want = sh.config.read().exclusive;
        if want != grabbed && !(want && grab_failed) {
            let idle = dev.get_key_state().map(|s| s.iter().next().is_none()).unwrap_or(true);
            if idle {
                if want {
                    match dev.grab() {
                        Ok(()) => grabbed = true,
                        Err(e) => {
                            grab_failed = true;
                            sh.log(format!(
                                "Could not grab {} ({e}); is another remapper running?",
                                path.display()
                            ));
                        }
                    }
                } else {
                    let _ = dev.ungrab();
                    grabbed = false;
                }
                set_grabbed(&sh, &path, grabbed);
            }
        }
        if !want {
            grab_failed = false;
        }

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd, 1, 100) };
        if r == 0 {
            continue;
        }
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            break;
        }
        let evs: Vec<InputEvent> = match dev.fetch_events() {
            Ok(it) => it.collect(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => break,
        };
        for ev in evs {
            match ev.event_type() {
                EventType::SYNCHRONIZATION => {
                    if grabbed && !batch.is_empty() {
                        sh.emit(&batch);
                    }
                    batch.clear();
                }
                EventType::KEY => {
                    match ev.value() {
                        1 => {
                            down.insert(ev.code());
                        }
                        0 => {
                            down.remove(&ev.code());
                        }
                        _ => {}
                    }
                    if sh.on_key(ev.code(), ev.value()) && grabbed {
                        batch.push(ev);
                    }
                }
                EventType::RELATIVE => {
                    sh.on_rel(ev.code(), ev.value());
                    if grabbed {
                        batch.push(ev);
                    }
                }
                _ => {}
            }
        }
    }

    let mut rel = Vec::new();
    for code in down {
        if sh.on_key(code, 0) && grabbed {
            rel.push(key_ev(code, 0));
        }
    }
    sh.emit(&rel);
    sh.log(format!("Device removed: {}", path.display()));
}
