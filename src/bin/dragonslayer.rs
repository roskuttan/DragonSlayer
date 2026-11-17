//! DragonSlayer — desktop app replacing MSI Dragon Center.
//!
//! A worker thread owns the hardware (one EC access at a time). It polls the
//! EC once a second and applies commands sent from the window. Everything
//! you see is custom-painted: gauges, chart, segmented controls, switch.
//!
//! `dragonslayer --mock` runs against a fake EC.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontData, FontFamily, FontId, Id, Mesh, Painter, Pos2, Rect,
    Sense, Shape, Stroke, StrokeKind, Ui, ViewportCommand, pos2, vec2,
};
use dragonslayer::automation::{Action, Automation};
use dragonslayer::autostart;
use dragonslayer::controller::{Controller, Status};
use dragonslayer::ec::backend::{EcBackend, EcError, EcResult, MockEc};
use dragonslayer::ec::map::{ChargeLimit, FanCurve, FanMode, KbdBacklight, CURVE_POINTS};
use dragonslayer::ec::open_hardware;
use dragonslayer::power::{self, Level};
use dragonslayer::settings::{HEAT_LIMITS, Refresh, Settings};
use dragonslayer::{display, history, smart, sysinfo, winkey};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

// ------------------------------------------------------------------ design

const WIN_W: f32 = 1040.0;
const WIN_H: f32 = 600.0;
const PAD: f32 = 16.0;
const GAP: f32 = 10.0;
const RADIUS: f32 = 14.0;

const BG: Color32 = Color32::from_rgb(10, 11, 15);
const CARD: Color32 = Color32::from_rgb(19, 21, 27);
const BORDER: Color32 = Color32::from_rgb(33, 36, 45);
const TRACK: Color32 = Color32::from_rgb(31, 34, 43);
const WELL: Color32 = Color32::from_rgb(13, 14, 19);
const TEXT: Color32 = Color32::from_rgb(238, 240, 245);
const DIM: Color32 = Color32::from_rgb(146, 151, 165);
const FAINT: Color32 = Color32::from_rgb(92, 97, 111);
const ACCENT: Color32 = Color32::from_rgb(232, 36, 60);
const COOL: Color32 = Color32::from_rgb(52, 214, 146);
const WARM: Color32 = Color32::from_rgb(255, 190, 70);
const HOT: Color32 = Color32::from_rgb(255, 72, 92);
const CPU_C: Color32 = Color32::from_rgb(255, 112, 72);
const GPU_C: Color32 = Color32::from_rgb(70, 190, 255);

const T_MIN: f32 = 30.0;
const T_MAX: f32 = 100.0;
const HISTORY: usize = 120; // seconds in the chart

fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}
fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Green -> amber -> red along 0..1.
fn heat(f: f32) -> Color32 {
    if f < 0.5 { mix(COOL, WARM, f * 2.0) } else { mix(WARM, HOT, (f - 0.5) * 2.0) }
}

fn temp_frac(t: f32) -> f32 {
    ((t - T_MIN) / (T_MAX - T_MIN)).clamp(0.0, 1.0)
}

// ------------------------------------------------------------------ worker

enum Cmd {
    Fan(FanMode),
    /// Save + apply a custom curve and switch to Custom (Advanced) mode.
    Curve(FanCurve),
    Boost(bool),
    Charge(u8),
    Kbd(u8),
    Webcam(bool),
    Plan(power::Guid),
    Scenario(Scenario),
    /// Only saves the choice; the UI thread flips the hook itself.
    WinLock(bool),
    HeatProtect(bool),
    HeatLimit(u8),
    BatteryQuiet(bool),
    BatteryKbdOff(bool),
    CloseToTray(bool),
    /// Smart fan mode on / off
    Smart(bool),
    /// Smart mode preference in degrees (+ quieter, - cooler)
    SmartBias(i8),
    /// Throw away everything Smart mode has learned
    SmartForget,
    Refresh(Refresh),
}

/// One-click presets, like Dragon Center's "User Scenario" page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Silent,
    Balanced,
    Performance,
    Extreme,
}

impl Scenario {
    const ALL: [Scenario; 4] = [Self::Silent, Self::Balanced, Self::Performance, Self::Extreme];

    fn name(self) -> &'static str {
        match self {
            Self::Silent => "Silent",
            Self::Balanced => "Balanced",
            Self::Performance => "Performance",
            Self::Extreme => "Extreme",
        }
    }

    fn level(self) -> Level {
        match self {
            Self::Silent | Self::Balanced => Level::Balanced,
            Self::Performance | Self::Extreme => Level::Performance,
        }
    }

    fn fan(self) -> FanMode {
        if self == Self::Silent { FanMode::Silent } else { FanMode::Auto }
    }

    fn boost(self) -> bool {
        self == Self::Extreme
    }

    fn fans_text(self) -> &'static str {
        match self {
            Self::Silent => "Silent fan",
            Self::Balanced | Self::Performance => "Auto fan",
            Self::Extreme => "Cooler Boost",
        }
    }

    /// The plan this scenario uses (falls back to Balanced if there's no
    /// performance plan on this PC).
    fn plan(self, plans: &[power::Plan]) -> Option<&power::Plan> {
        power::pick(plans, self.level()).or_else(|| power::pick(plans, Level::Balanced))
    }
}

/// System info gathered by the worker (no EC needed).
#[derive(Clone, Default)]
struct SysSnap {
    cpu_pct: Option<f32>,
    cpu_hist: VecDeque<f32>,
    mem: Option<(f32, f32)>,
    gpu: Option<sysinfo::Gpu>,
    battery: Option<sysinfo::Battery>,
    power: Option<(bool, u8)>,
}

#[derive(Default)]
struct Shared {
    status: Option<Status>,
    history: VecDeque<(u8, u8)>,
    error: Option<String>,
    message: Option<(String, bool, Instant)>,
    can_write: bool,
    dragon_center_running: bool,
    backend: &'static str,
    sys: SysSnap,
    settings: Settings,
    auto_boosted: bool,
    plans: Vec<power::Plan>,
    active_plan: Option<power::Guid>,
    camera: bool,
    /// Smart fan mode: what it decided this second (None while it is off)
    smart: Option<smart::Decision>,
    brain: BrainView,
    /// screen refresh rate now, and the rates the screen offers
    hz: Option<u32>,
    rates: Vec<u32>,
}

/// What the Smart panel shows of the learned data.
#[derive(Clone, Default)]
struct BrainView {
    map: [[smart::Cell; smart::STEPS]; smart::LOADS],
    /// (program, usual load level, hours seen), heaviest first
    apps: Vec<(String, usize, f32)>,
    learned_secs: f64,
    drift_c: Option<f32>,
}

impl BrainView {
    fn of(b: &smart::Brain) -> Self {
        Self { map: b.map, apps: b.known_apps(), learned_secs: b.learned_secs, drift_c: b.drift_c() }
    }
}

/// Demo mode: a few hours of made-up learning, so the Smart panel has
/// something to show.
const DEMO_LEARNED: &str = "v1
learned_secs 15120
cell 0 0 67.8 400 67.2
cell 0 1 66.9 40 66.5
cell 1 0 70.9 220 70.1
cell 1 1 69.6 60 69.6
cell 1 2 68.8 12 0
cell 2 3 78.1 90 77.5
cell 2 4 76.6 150 76.0
cell 2 5 75.4 30 75.4
cell 2 7 73.9 14 0
cell 3 6 86.2 80 85.1
cell 3 7 84.0 200 83.2
cell 4 7 90.3 45 89.8
app 9200 78 valorant.exe
app 5400 66 blender.exe
app 30000 24 code.exe
app 80000 11 chrome.exe
";

/// `open` connects to the hardware and names the backend. If connecting
/// fails, the worker keeps retrying every 2 seconds instead of giving up.
fn spawn_worker<B: EcBackend + 'static>(
    open: impl Fn() -> EcResult<(B, &'static str)> + Send + 'static,
    shared: Arc<Mutex<Shared>>,
    rx: mpsc::Receiver<Cmd>,
    ctx: egui::Context,
    persist: bool,
) {
    std::thread::spawn(move || {
        let (mut settings, load_err) = if persist { Settings::load_checked() } else { (Settings::default(), None) };
        if persist && let Some(p) = Settings::path() {
            // start report: did reading and writing the settings folder work?
            // (readable without admin rights, for finding problems later)
            let wrote = Settings::write_file(&p.with_file_name("write-test.txt"), "ok");
            let report = format!(
                "settings read: {}\nsettings folder write: {}\nsmart={} fan={:?} heat_protect={}\n",
                load_err.as_deref().unwrap_or("ok"),
                wrote.as_ref().map_or_else(|e| e.to_string(), |()| "ok".into()),
                settings.smart,
                settings.fan,
                settings.heat_protect
            );
            let _ = std::fs::remove_file(p.with_file_name("write-test.txt"));
            let _ = std::fs::write(p.with_file_name("last-start.txt"), report);
        }
        if let Some(e) = &load_err {
            shared.lock().unwrap().message = Some((format!("Couldn't read your saved settings ({e})"), false, Instant::now()));
        }
        shared.lock().unwrap().settings = settings.clone();
        let mut cpu = sysinfo::CpuMeter::default();
        cpu.sample();

        let mut ctl = loop {
            match open().and_then(|(b, name)| Ok((Controller::new(b)?, name))) {
                Ok((c, name)) => {
                    let mut s = shared.lock().unwrap();
                    s.backend = name;
                    s.error = None;
                    break c;
                }
                Err(e) => {
                    shared.lock().unwrap().error = Some(format!("{e} (retrying...)"));
                    ctx.request_repaint();
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        };
        shared.lock().unwrap().can_write = ctl.can_write();

        // Re-apply what the user chose last time (like Dragon Center at login).
        if ctl.can_write() && !settings.no_hardware() {
            let (n, errs) = settings.apply(&mut ctl);
            shared.lock().unwrap().message = Some(if errs.is_empty() {
                (format!("Restored your {n} saved settings"), true, Instant::now())
            } else {
                (format!("Restore failed: {}", errs.join("; ")), false, Instant::now())
            });
        }

        let mut auto = Automation::default();
        auto.prime(sysinfo::power().map(|p| p.0));
        // Smart fan mode: what it has learned so far (kept next to the settings)
        let learned_path = if persist { Settings::learned_path() } else { None };
        let mut brain = match &learned_path {
            Some(p) => std::fs::read_to_string(p).map(|t| smart::Brain::parse(&t)).unwrap_or_default(),
            None => smart::Brain::parse(DEMO_LEARNED),
        };
        let mut brain_saved = Instant::now();
        let save_brain = |b: &smart::Brain| {
            if let Some(p) = &learned_path {
                let _ = Settings::write_file(p, &b.to_text());
            }
        };
        let (mut cpu_load, mut gpu_load) = (0.0f32, 0.0f32);
        let mut last_tick = Instant::now();
        // history log (real runs only; the demo writes nothing)
        let mut log = if persist { Settings::path().map(|p| history::Log::new(p.with_file_name("history"))) } else { None };
        let mut switcher = display::Switcher::default();
        let screen_rates = if persist { display::rates() } else { vec![48, 144] };
        let mut tick: u32 = 0;
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(cmd) => {
                    let was = settings.refresh;
                    let forget = matches!(cmd, Cmd::SmartForget);
                    let (what, result) = apply(&mut ctl, cmd, &mut settings, &mut brain, persist);
                    if forget {
                        save_brain(&brain);
                    }
                    // refresh switching turned off: give the fastest rate back
                    if persist && was != Refresh::Off && settings.refresh == Refresh::Off && let Some(&fast) = screen_rates.last() {
                        let _ = display::set_hz(fast);
                    }
                    // a choice that can't be saved must not look saved
                    let saved = if result.is_ok() && persist { settings.save() } else { Ok(()) };
                    let msg = match (result, saved) {
                        (Ok(()), Ok(())) => (what, true, Instant::now()),
                        (Ok(()), Err(e)) => (format!("{what}, but it could not be saved: {e}"), false, Instant::now()),
                        (Err(e), _) => (format!("{what} failed: {e}"), false, Instant::now()),
                    };
                    let mut s = shared.lock().unwrap();
                    s.settings = settings.clone();
                    s.message = Some(msg);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }

            // system info (GPU every 3 s, battery every 10 s: they're slower)
            let power = sysinfo::power();
            let cpu_pct = cpu.sample();
            let mem = sysinfo::memory();
            let gpu = tick.is_multiple_of(3).then(sysinfo::gpu);
            let battery = tick.is_multiple_of(10).then(sysinfo::battery);
            let dc = tick.is_multiple_of(10).then(dragon_center_running);
            let plans = (persist && tick.is_multiple_of(10)).then(power::plans);
            let active_plan = if persist { power::active() } else { None };
            let camera = tick.is_multiple_of(3).then(sysinfo::camera_present);
            tick = tick.wrapping_add(1);

            let mut status = ctl.status();
            // demo: make the readings move every second like a real laptop's.
            // (Still demo data hid a bug where each change of a live reading
            // made the UI redraw at the full refresh rate.)
            if !persist && let Ok(st) = &mut status {
                let wobble = |salt: u32, span: u32| (tick.wrapping_mul(2_654_435_761).rotate_left(salt) % span) as u8;
                st.cpu_temp_c += wobble(3, 6);
                st.gpu_temp_c += wobble(7, 3);
                st.fan_rpm += u16::from(wobble(11, 120));
            }

            // automation: heat protection, on-battery profile
            let mut auto_msg = None;
            if let (Ok(st), true) = (&status, ctl.can_write()) {
                let mut actions = auto.tick(&settings, st, power.map(|p| p.0));
                // Smart mode handles battery itself: no switch to Silent
                actions.retain(|(a, _)| !(settings.smart && matches!(a, Action::Fan(_))));
                for (act, why) in &actions {
                    let r: EcResult<()> = match act {
                        Action::Boost(b) => ctl.set_cooler_boost(*b),
                        Action::Fan(f) => ctl.set_fan_profile(*f, settings.curve),
                        Action::Kbd(l) => KbdBacklight::new(*l).map_or(Ok(()), |k| ctl.set_kbd_backlight(k)),
                        Action::RestoreUser => {
                            let (_, errs) = settings.apply(&mut ctl);
                            if errs.is_empty() { Ok(()) } else { Err(EcError::Io(errs.join("; "))) }
                        }
                    };
                    auto_msg = Some(match r {
                        Ok(()) => (why.clone(), true),
                        Err(e) => (format!("{why} failed: {e}"), false),
                    });
                }
                if !actions.is_empty() {
                    status = ctl.status();
                }
            }

            // Smart fan mode: one decision a second; write the curve only
            // when the laptop's doesn't match it
            let dt = last_tick.elapsed().as_secs_f32();
            last_tick = Instant::now();
            if let Some(c) = cpu_pct {
                cpu_load = c;
            }
            if let Some(Some(g)) = &gpu {
                gpu_load = g.util_pct;
            }
            let mut decision = None;
            if settings.smart && let Ok(st) = &status {
                let sample = smart::Sample {
                    cpu_temp: f32::from(st.cpu_temp_c),
                    gpu_temp: f32::from(st.gpu_temp_c),
                    cpu_load,
                    gpu_load,
                    on_battery: power.is_some_and(|p| !p.0),
                    app: if persist { sysinfo::foreground_app() } else { Some("chrome.exe".into()) },
                };
                let d = brain.tick(&sample, dt, f32::from(settings.smart_bias));
                let want = smart::curve_for(d.fan_pct);
                let differs = st.fan_mode != Ok(FanMode::Advanced) || st.cpu_curve != want.speeds() || st.gpu_curve != want.speeds();
                decision = Some(d);
                if differs && ctl.can_write() {
                    match ctl.set_fan_profile(FanMode::Advanced, Some(want)) {
                        Ok(()) => status = ctl.status(),
                        Err(e) => auto_msg = Some((format!("Smart mode: {e}"), false)),
                    }
                }
                if brain_saved.elapsed() >= Duration::from_secs(120) {
                    save_brain(&brain);
                    brain_saved = Instant::now();
                }
            }

            // history: one row every few seconds, to compare modes and tune later
            if tick.is_multiple_of(history::EVERY_SECS)
                && let (Some(log), Ok(st)) = (&mut log, &status)
            {
                let mode = match st.fan_mode {
                    _ if settings.smart => "smart",
                    Ok(FanMode::Silent) => "silent",
                    Ok(FanMode::Advanced) => "custom",
                    _ => "auto",
                };
                log.append(&history::Row {
                    mode,
                    cpu_c: st.cpu_temp_c,
                    gpu_c: st.gpu_temp_c,
                    cpu_load,
                    gpu_load,
                    fan_target_pct: st.cpu_fan_pct.max(st.gpu_fan_pct),
                    rpm: st.fan_rpm,
                    on_battery: power.is_some_and(|p| !p.0),
                    boost: st.cooler_boost,
                    smart: decision.as_ref().map(|d| (d.fan_pct, d.load_level)),
                });
            }

            // screen refresh rate by situation
            let hz = if persist { display::current_hz() } else { Some(144) };
            if persist && let (Some(cur), Some((plugged, _))) = (hz, power)
                && let Some(to) = switcher.decide(settings.refresh, &screen_rates, cur, plugged, gpu_load >= 35.0, dt)
            {
                auto_msg = Some(match display::set_hz(to) {
                    Ok(()) => (format!("Screen: {to} Hz ({})", if plugged { "plugged in" } else if to == screen_rates[0] { "on battery" } else { "game on battery" }), true),
                    Err(e) => (format!("Screen refresh: {e}"), false),
                });
            }

            let mut s = shared.lock().unwrap();
            s.smart = decision;
            if settings.smart || tick.is_multiple_of(10) {
                s.brain = BrainView::of(&brain);
            }
            s.hz = hz;
            if s.rates.is_empty() {
                s.rates.clone_from(&screen_rates);
            }
            if let Some(dc) = dc {
                s.dragon_center_running = dc;
            }
            if let Some(p) = plans {
                s.plans = p;
            }
            s.active_plan = active_plan;
            if let Some(c) = camera {
                s.camera = c;
            }
            if let Some((m, ok)) = auto_msg {
                s.message = Some((m, ok, Instant::now()));
            }
            s.auto_boosted = auto.auto_boosted();
            s.sys.power = power;
            s.sys.mem = mem;
            if let Some(c) = cpu_pct {
                s.sys.cpu_pct = Some(c);
                s.sys.cpu_hist.push_back(c);
                while s.sys.cpu_hist.len() > 60 {
                    s.sys.cpu_hist.pop_front();
                }
            }
            if let Some(g) = gpu {
                s.sys.gpu = g;
            }
            if let Some(b) = battery {
                s.sys.battery = b;
            }
            match status {
                Ok(st) => {
                    s.history.push_back((st.cpu_temp_c, st.gpu_temp_c));
                    while s.history.len() > HISTORY {
                        s.history.pop_front();
                    }
                    s.status = Some(st);
                    s.error = None;
                }
                Err(e) => s.error = Some(e.to_string()),
            }
            drop(s);
            ctx.request_repaint();
        }
    });
}

fn fan_name(m: FanMode) -> &'static str {
    match m {
        FanMode::Auto => "Auto",
        FanMode::Silent => "Silent",
        FanMode::Basic => "Basic",
        FanMode::Advanced => "Custom",
    }
}

/// Applies a command and, on success, records it in `settings`.
/// `real` is false in demo mode: nothing outside the fake EC is touched.
fn apply<B: EcBackend>(ctl: &mut Controller<B>, cmd: Cmd, settings: &mut Settings, brain: &mut smart::Brain, real: bool) -> (String, Result<(), String>) {
    let r = |x: EcResult<()>| x.map_err(|e| e.to_string());
    match cmd {
        Cmd::Fan(m) => {
            let res = r(ctl.set_fan_profile(m, settings.curve));
            if res.is_ok() {
                settings.fan = Some(m);
                settings.smart = false;
            }
            (format!("Fan set to {}", fan_name(m)), res)
        }
        Cmd::Curve(c) => {
            let res = r(ctl.set_fan_profile(FanMode::Advanced, Some(c)));
            if res.is_ok() {
                settings.fan = Some(FanMode::Advanced);
                settings.smart = false;
                settings.curve = Some(c);
            }
            ("Custom fan curve applied".to_string(), res)
        }
        Cmd::Webcam(on) => (format!("Webcam power {}", if on { "on" } else { "off" }), r(ctl.set_webcam(on))),
        Cmd::Plan(g) => {
            let name = power::plans().into_iter().find(|p| p.guid == g).map_or("?".into(), |p| p.name);
            let res = if real { power::activate(&g) } else { Ok(()) };
            (format!("Power plan: {name}"), res)
        }
        Cmd::Scenario(sc) => {
            let mut errs = Vec::new();
            if real {
                let plans = power::plans();
                match sc.plan(&plans) {
                    Some(p) => {
                        if let Err(e) = power::activate(&p.guid) {
                            errs.push(e);
                        }
                    }
                    None => errs.push("no matching power plan".into()),
                }
            }
            match ctl.set_fan_profile(sc.fan(), settings.curve) {
                Ok(()) => {
                    settings.fan = Some(sc.fan());
                    settings.smart = false;
                }
                Err(e) => errs.push(e.to_string()),
            }
            match ctl.set_cooler_boost(sc.boost()) {
                Ok(()) => settings.boost = Some(sc.boost()),
                Err(e) => errs.push(e.to_string()),
            }
            (format!("Scenario: {}", sc.name()), if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) })
        }
        Cmd::WinLock(on) => {
            settings.win_lock = on;
            (format!("Windows key {}", if on { "locked" } else { "unlocked" }), Ok(()))
        }
        Cmd::Boost(on) => {
            let res = r(ctl.set_cooler_boost(on));
            if res.is_ok() {
                settings.boost = Some(on);
            }
            (format!("Cooler Boost {}", if on { "on" } else { "off" }), res)
        }
        Cmd::Charge(p) => {
            let res = ChargeLimit::new(p).map_or(Err("invalid percent".into()), |c| r(ctl.set_charge_limit(c)));
            if res.is_ok() {
                settings.charge = Some(p);
            }
            (format!("Battery limit set to {p}%"), res)
        }
        Cmd::Kbd(l) => {
            let res = KbdBacklight::new(l).map_or(Err("invalid level".into()), |k| r(ctl.set_kbd_backlight(k)));
            if res.is_ok() {
                settings.kbd = Some(l);
            }
            (format!("Keyboard light set to {}", ["off", "low", "mid", "high"][l.min(3) as usize]), res)
        }
        Cmd::HeatProtect(on) => {
            settings.heat_protect = on;
            (format!("Heat protection {}", if on { "on" } else { "off" }), Ok(()))
        }
        Cmd::HeatLimit(l) => {
            settings.heat_limit = l;
            (format!("Heat protection limit {l}°C"), Ok(()))
        }
        Cmd::BatteryQuiet(on) => {
            settings.battery_quiet = on;
            (format!("Quiet fan on battery {}", if on { "on" } else { "off" }), Ok(()))
        }
        Cmd::BatteryKbdOff(on) => {
            settings.battery_kbd_off = on;
            (format!("Keyboard light off on battery {}", if on { "on" } else { "off" }), Ok(()))
        }
        Cmd::CloseToTray(on) => {
            settings.close_to_tray = on;
            (format!("Close to tray {}", if on { "on" } else { "off" }), Ok(()))
        }
        Cmd::Smart(on) => {
            let res = if on {
                r(ctl.set_fan_profile(FanMode::Advanced, Some(smart::curve_for(50))))
            } else {
                // back to what the user had before
                r(ctl.set_fan_profile(settings.fan.unwrap_or(FanMode::Auto), settings.curve))
            };
            if res.is_ok() {
                settings.smart = on;
            }
            (format!("Smart fan mode {}", if on { "on" } else { "off" }), res)
        }
        Cmd::SmartBias(b) => {
            settings.smart_bias = b.clamp(-6, 6);
            ("Smart mode preference saved".to_string(), Ok(()))
        }
        Cmd::SmartForget => {
            *brain = smart::Brain::default();
            ("Smart mode forgot everything it learned".to_string(), Ok(()))
        }
        Cmd::Refresh(p) => {
            settings.refresh = p;
            let what = match p {
                Refresh::Off => "Screen refresh: left alone",
                Refresh::Battery => "Screen refresh: slow on battery",
                Refresh::Adaptive => "Screen refresh: adaptive",
            };
            (what.to_string(), Ok(()))
        }
    }
}

/// True if Dragon Center's app or its central service is running.
fn dragon_center_running() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    ["DCv2.exe", "MSI_Central_Service.exe"].iter().any(|name| {
        std::process::Command::new(dragonslayer::elevation::system_dir().join("tasklist.exe"))
            .args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(name))
            .unwrap_or(false)
    })
}

// -------------------------------------------------- Windows 11 round corners

mod win {
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicIsize, Ordering};

    #[repr(C)]
    #[derive(Clone, Copy, Default, PartialEq)]
    struct Rect {
        l: i32,
        t: i32,
        r: i32,
        b: i32,
    }
    #[repr(C)]
    struct MonitorInfo {
        size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const c_void, size: u32) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn GetWindowRect(hwnd: isize, r: *mut Rect) -> i32;
        fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
        fn GetMonitorInfoW(mon: isize, info: *mut MonitorInfo) -> i32;
        fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn GetCursorPos(p: *mut [i32; 2]) -> i32;
    }

    static FOUND: AtomicIsize = AtomicIsize::new(0);
    /// Window rect from before we maximised; Some = maximised.
    static RESTORE: Mutex<Option<Rect>> = Mutex::new(None);

    extern "system" fn find(hwnd: isize, _: isize) -> i32 {
        let mut pid = 0u32;
        // SAFETY: plain queries on a handle EnumWindows just gave us.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == std::process::id() && IsWindowVisible(hwnd) != 0 {
                FOUND.store(hwnd, Ordering::Relaxed);
                return 0; // stop
            }
        }
        1
    }

    /// Our visible top-level window (the tray's helper windows are hidden).
    fn own_window() -> Option<isize> {
        FOUND.store(0, Ordering::Relaxed);
        // SAFETY: `find` matches the WNDENUMPROC signature.
        unsafe { EnumWindows(find, 0) };
        Some(FOUND.load(Ordering::Relaxed)).filter(|&h| h != 0)
    }

    fn corners(h: isize, round: bool) {
        const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
        let v: u32 = if round { 2 } else { 1 }; // DWMWCP_ROUND / DONOTROUND
        // SAFETY: valid window handle, 4-byte attribute value.
        unsafe { DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, (&v as *const u32).cast(), 4) };
    }

    fn place(h: isize, r: Rect) {
        const SWP_NOZORDER: u32 = 0x4;
        const SWP_NOACTIVATE: u32 = 0x10;
        // SAFETY: moving/resizing our own window.
        unsafe { SetWindowPos(h, 0, r.l, r.t, r.r - r.l, r.b - r.t, SWP_NOZORDER | SWP_NOACTIVATE) };
    }

    /// Ask Windows 11 to round our borderless window's corners.
    pub fn round_corners() {
        if let Some(h) = own_window() {
            corners(h, true);
        }
    }

    pub fn is_maximised() -> bool {
        RESTORE.lock().unwrap().is_some()
    }

    /// Our own maximise: fill the monitor's work area. Windows' real maximise
    /// makes a borderless window cover the WHOLE monitor (with an auto-hide
    /// taskbar), which Windows treats as a full-screen game and flips in and
    /// out of composition -> the window blinks, and the auto-hide taskbar
    /// can't pop up. Leaving one pixel free avoids both.
    pub fn set_maximised(on: bool) {
        let Some(h) = own_window() else { return };
        let mut saved = RESTORE.lock().unwrap();
        match (on, *saved) {
            (true, None) => {
                let mut cur = Rect::default();
                let mut mi = MonitorInfo { size: size_of::<MonitorInfo>() as u32, monitor: Rect::default(), work: Rect::default(), flags: 0 };
                // SAFETY: out-pointers to properly sized structs.
                let ok = unsafe { GetWindowRect(h, &mut cur) != 0 && GetMonitorInfoW(MonitorFromWindow(h, 2), &mut mi) != 0 };
                if !ok {
                    return;
                }
                let mut r = mi.work;
                if r == mi.monitor {
                    r.b -= 1; // auto-hide taskbar: stay off "full screen"
                }
                *saved = Some(cur);
                corners(h, false);
                place(h, r);
            }
            (false, Some(old)) => {
                *saved = None;
                place(h, old);
                corners(h, true);
            }
            _ => {}
        }
    }

    /// Dragging the title bar of the maximised window: restore it to its old
    /// size under the cursor (like Windows does), so the drag moves a window
    /// of normal size.
    pub fn restore_under_cursor() {
        let Some(h) = own_window() else { return };
        let Some(old) = RESTORE.lock().unwrap().take() else { return };
        let mut cur = Rect::default();
        let mut p = [0i32; 2];
        // SAFETY: out-pointers to properly sized values.
        if unsafe { GetWindowRect(h, &mut cur) == 0 || GetCursorPos(&mut p) == 0 } {
            return;
        }
        let (w, ht) = (old.r - old.l, old.b - old.t);
        let frac = (p[0] - cur.l) as f32 / (cur.r - cur.l).max(1) as f32;
        let l = p[0] - (w as f32 * frac) as i32;
        let t = p[1] - (p[1] - cur.t).min(ht / 2);
        place(h, Rect { l, t, r: l + w, b: t + ht });
        corners(h, true);
    }
}

// --------------------------------------------------------------------- app

/// The app needs admin rights (PawnIO). True if THIS process should run it;
/// false if it handed off: a copy already running shows itself, or an admin
/// copy is being started (task "DragonSlayer Open", else one UAC prompt).
fn launch_or_continue(args: &[String]) -> bool {
    use dragonslayer::elevation;
    // we were clicked, so we may pass the right to come to the front on
    elevation::allow_foreground();
    if elevation::signal_existing() {
        return false;
    }
    if elevation::is_elevated() {
        return true;
    }
    if autostart::run_open_task() && elevation::wait_for_running(Duration::from_secs(6)) {
        return false;
    }
    // no task yet (first run, or not installed under Program Files): ask once
    elevation::relaunch_as_admin(args);
    false
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mock = args.iter().any(|a| a == "--mock");
    // Started by the logon task: restore settings quietly, stay minimized.
    let startup = args.iter().any(|a| a == "--startup");
    if !mock && !launch_or_continue(&args) {
        return Ok(());
    }
    // a later click on the icon asks this copy to show its window
    let show_req = Arc::new(AtomicBool::new(false));
    let ctx_slot: Arc<std::sync::OnceLock<egui::Context>> = Arc::new(std::sync::OnceLock::new());
    if !mock {
        let (flag, slot) = (show_req.clone(), ctx_slot.clone());
        dragonslayer::elevation::listen_for_show(move || {
            flag.store(true, Ordering::Relaxed);
            if let Some(ctx) = slot.get() {
                ctx.request_repaint();
            }
        });
    }
    let icon = egui::IconData { rgba: dragonslayer::icon::rgba(256), width: 256, height: 256 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DragonSlayer")
            .with_inner_size([WIN_W, WIN_H])
            .with_decorations(false)
            .with_resizable(false)
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "DragonSlayer",
        options,
        Box::new(move |cc| {
            let _ = ctx_slot.set(cc.egui_ctx.clone());
            let mut app = App::new(cc, mock, startup);
            app.show_req = show_req;
            Ok(Box::new(app))
        }),
    )
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let load = |f: &str| std::fs::read(format!(r"C:\Windows\Fonts\{f}")).ok();
    if let Some(b) = load("segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), Arc::new(FontData::from_owned(b)));
        if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
            list.insert(0, "segoe".into());
        }
    }
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (key, file) in [("semibold", "seguisb.ttf"), ("bold", "segoeuib.ttf")] {
        let mut list = Vec::new();
        if let Some(b) = load(file) {
            fonts.font_data.insert(key.into(), Arc::new(FontData::from_owned(b)));
            list.push(key.to_string());
        }
        list.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(key.into()), list);
    }
    ctx.set_fonts(fonts);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Fans,
    Tools,
    System,
    Automation,
}

/// What the tray reports back (delivered from the tray's own callbacks).
enum TrayMsg {
    Show,
    Menu(MenuId),
}

struct TrayMenu {
    open: MenuId,
    boost: CheckMenuItem,
    silent: CheckMenuItem,
    quit: MenuId,
}

/// Tray icon with a quick menu. Clicks wake the UI immediately, even while
/// the window is hidden.
fn make_tray(ctx: &egui::Context) -> Option<(TrayIcon, TrayMenu, mpsc::Receiver<TrayMsg>)> {
    let open = MenuItem::new("Open DragonSlayer", true, None);
    let boost = CheckMenuItem::new("Cooler Boost", true, false, None);
    let silent = CheckMenuItem::new("Silent fan", true, false, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    for item in [&open as &dyn tray_icon::menu::IsMenuItem, &PredefinedMenuItem::separator(), &boost, &silent, &PredefinedMenuItem::separator(), &quit] {
        menu.append(item).ok()?;
    }
    let icon = tray_icon::Icon::from_rgba(dragonslayer::icon::rgba(32), 32, 32).ok()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("DragonSlayer")
        .with_icon(icon)
        .build()
        .ok()?;

    let (tx, rx) = mpsc::channel();
    let (tx2, c1, c2) = (tx.clone(), ctx.clone(), ctx.clone());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        let show = matches!(
            e,
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
        );
        if show {
            let _ = tx.send(TrayMsg::Show);
            c1.request_repaint();
        }
    }));
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = tx2.send(TrayMsg::Menu(e.id));
        c2.request_repaint();
    }));
    let ids = TrayMenu { open: open.id().clone(), boost, silent, quit: quit.id().clone() };
    Some((tray, ids, rx))
}

struct App {
    /// set when another launch of the app asks this copy to show itself
    show_req: Arc<AtomicBool>,
    /// window hidden in the tray: no animation frames then
    hidden: bool,
    /// Dashboard fan: current angle (rad)
    fan_angle: f32,
    /// DS_FRAME_LOG diagnostics: frames, summed CPU seconds, window start
    frame_log: (u32, f32, f64),
    /// when the current tab was opened (for its entrance animation)
    tab_since: f64,
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<Cmd>,
    mock: bool,
    frames: u32,
    autostart: bool,
    startup: bool,
    tab: Tab,
    tray: Option<(TrayIcon, TrayMenu, mpsc::Receiver<TrayMsg>)>,
    quitting: bool,
    last_tip: String,
    /// Fan curve being edited (None = showing the saved one).
    draft: Option<[u8; CURVE_POINTS]>,
    /// Windows-key hook installed.
    winkey_ok: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, mock: bool, startup: bool) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        setup_fonts(&cc.egui_ctx);
        let tray = make_tray(&cc.egui_ctx);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        if mock {
            spawn_worker(|| Ok((MockEc::gf63_idle(), "Demo")), shared.clone(), rx, ctx, false);
        } else {
            let open = || {
                open_hardware("auto").map(|h| {
                    let name = h.name();
                    (h, name)
                })
            };
            spawn_worker(open, shared.clone(), rx, ctx, true);
        }
        // also moves an enabled task over from the old name / an old exe path
        let autostart = !mock && std::env::current_exe().is_ok_and(|exe| autostart::refresh(&exe));
        // keep the no-prompt "DragonSlayer Open" task pointing here (or gone)
        if !mock && let Ok(exe) = std::env::current_exe() {
            std::thread::spawn(move || autostart::sync_open_task(&exe));
        }
        let winkey_ok = !mock && winkey::start();
        if winkey_ok {
            winkey::set_locked(Settings::load().win_lock);
        }
        Self {
            shared,
            tx,
            mock,
            frames: 0,
            autostart,
            startup,
            // `--tab system|automation` opens on that tab (handy for screenshots)
            tab: match std::env::args().skip_while(|a| a != "--tab").nth(1).as_deref() {
                Some("fans") => Tab::Fans,
                Some("tools") => Tab::Tools,
                Some("system") => Tab::System,
                Some("automation") => Tab::Automation,
                _ => Tab::Dashboard,
            },
            tray,
            quitting: false,
            last_tip: String::new(),
            draft: None,
            winkey_ok,
            show_req: Arc::new(AtomicBool::new(false)),
            hidden: false,
            fan_angle: 0.0,
            frame_log: (0, 0.0, 0.0),
            tab_since: -10.0,
        }
    }

    fn show_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn close_to_tray(&self) -> bool {
        self.tray.is_some() && self.shared.lock().unwrap().settings.close_to_tray
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// The fan choices as one control: Auto / Silent / Custom / Smart.
    fn fan_choice(&mut self, ui: &mut Ui, r: Rect, id: &str, st: &Status, smart_on: bool, can_write: bool) {
        let modes = [FanMode::Auto, FanMode::Silent, FanMode::Advanced];
        let sel = if smart_on { Some(3) } else { st.fan_mode.ok().and_then(|m| modes.iter().position(|&x| x == m)) };
        if let Some(i) = segmented(ui, r, Id::new(id), &["Auto", "Silent", "Custom", "Smart"], sel, can_write) {
            self.send(if i == 3 { Cmd::Smart(true) } else { Cmd::Fan(modes[i]) });
        }
    }

    /// Smart mode's panel: what it is doing and why, and what it has learned.
    fn smart_panel(&mut self, ui: &mut Ui, p: &Painter, ed: Rect, s: &Settings, can_write: bool) {
        let (now, bv) = {
            let sh = self.shared.lock().unwrap();
            (sh.smart.clone(), sh.brain.clone())
        };
        card_title(p, ed, "Smart mode");
        p.text(ed.left_top() + vec2(18.0, 38.0), Align2::LEFT_TOP, "Learns this laptop and sets the fan itself", regular(12.0), FAINT);
        let pr = Rect::from_min_size(pos2(ed.right() - 18.0 - 300.0, ed.top() + 14.0), vec2(300.0, 34.0));
        let psel = match s.smart_bias {
            b if b > 1 => 0,
            b if b < -1 => 2,
            _ => 1,
        };
        if let Some(i) = segmented(ui, pr, Id::new("smart_bias"), &["Quieter", "Balanced", "Cooler"], Some(psel), true) {
            self.send(Cmd::SmartBias([4, 0, -4][i]));
        }

        // right now: speed and the reason, in plain words
        let nowr = Rect::from_min_size(ed.left_top() + vec2(18.0, 68.0), vec2(ed.width() - 36.0, 62.0));
        p.rect_filled(nowr, 10.0, WELL);
        p.rect_stroke(nowr, 10.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
        match &now {
            Some(d) => {
                let g = p.layout_no_wrap(format!("{}%", d.fan_pct), bold(26.0), TEXT);
                let (gw, gh) = (g.size().x, g.size().y);
                p.galley(pos2(nowr.left() + 16.0, nowr.center().y - gh / 2.0), g, TEXT);
                let tx = nowr.left() + 16.0 + gw.max(62.0) + 14.0;
                p.text(pos2(tx, nowr.top() + 21.0), Align2::LEFT_CENTER, &d.reason, semibold(13.5), TEXT);
                let rpm = (f32::from(d.fan_pct) * FanCurve::RPM_PER_PCT / 10.0).round() as u32 * 10;
                let line = format!("steering by {:.0}°C  ·  aiming to stay under {:.0}°C  ·  about {} RPM", d.heat_c, d.target_c, thousands(rpm));
                p.text(pos2(tx, nowr.top() + 42.0), Align2::LEFT_CENTER, line, regular(12.0), DIM);
            }
            None => {
                p.text(nowr.center(), Align2::CENTER_CENTER, if can_write { "Starting..." } else { "Read-only: Smart mode can't set the fan" }, regular(12.5), FAINT);
            }
        }

        // the learned map: load level (rows) x fan speed (columns) -> temperature
        let top = nowr.bottom() + 34.0;
        p.text(pos2(ed.left() + 18.0, nowr.bottom() + 17.0), Align2::LEFT_CENTER, "WHAT IT HAS LEARNED", semibold(10.5), FAINT);
        p.text(pos2(ed.left() + 150.0, nowr.bottom() + 17.0), Align2::LEFT_CENTER, "temperature at each load and fan speed  ·  faint = its guess", regular(11.5), FAINT);
        let (x0, cw, ch, gap) = (ed.left() + 18.0 + 62.0, 46.0, 30.0, 4.0);
        let cur = now.as_ref().map(|d| (d.load_level, (usize::from(d.fan_pct.saturating_sub(30)) / 10).min(smart::STEPS - 1)));
        for level in 0..smart::LOADS {
            let y = top + (smart::LOADS - 1 - level) as f32 * (ch + gap);
            p.text(pos2(x0 - 10.0, y + ch / 2.0), Align2::RIGHT_CENTER, smart::LOAD_NAMES[level], regular(11.5), DIM);
            let est = smart::estimate_row(&bv.map[level], level);
            for (f, guess) in est.iter().enumerate() {
                let r = Rect::from_min_size(pos2(x0 + f as f32 * (cw + gap), y), vec2(cw, ch));
                let cell = bv.map[level][f];
                if cell.known() {
                    let col = heat(temp_frac(cell.temp));
                    p.rect_filled(r, 6.0, col.gamma_multiply(0.22));
                    p.text(r.center(), Align2::CENTER_CENTER, format!("{:.0}°", cell.temp), semibold(12.0), mix(col, TEXT, 0.55));
                } else {
                    p.rect_stroke(r, 6.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
                    // a guess far off the scale just means "too hot": say so
                    let txt = if *guess > 95.0 { "95°+".to_string() } else { format!("{guess:.0}°") };
                    p.text(r.center(), Align2::CENTER_CENTER, txt, regular(11.0), FAINT.gamma_multiply(0.7));
                }
                if cur == Some((level, f)) {
                    p.rect_stroke(r.expand(1.5), 7.0, Stroke::new(1.6, TEXT), StrokeKind::Inside);
                }
            }
        }
        let ly = top + smart::LOADS as f32 * (ch + gap) + 8.0;
        for f in 0..smart::STEPS {
            p.text(pos2(x0 + f as f32 * (cw + gap) + cw / 2.0, ly), Align2::CENTER_CENTER, format!("{}%", smart::step_pct(f)), regular(10.5), FAINT);
        }

        // what it knows, in numbers
        let sx = x0 + smart::STEPS as f32 * (cw + gap) + 22.0;
        let hours = bv.learned_secs / 3600.0;
        let learning = if hours >= 1.0 { format!("{hours:.1} h") } else { format!("{:.0} min", bv.learned_secs / 60.0) };
        let known = bv.map.iter().flatten().filter(|c| c.known()).count();
        let (cooling, ccol) = match bv.drift_c {
            Some(d) if d > 3.0 => (format!("{d:.0}°C hotter"), WARM),
            Some(d) if d < -3.0 => (format!("{:.0}°C cooler", -d), COOL),
            Some(_) => ("As first learned".to_string(), TEXT),
            None => ("Too early to say".to_string(), DIM),
        };
        let heavy: Vec<String> = bv.apps.iter().filter(|a| a.1 >= 2).take(2).map(|a| a.0.trim_end_matches(".exe").to_string()).collect();
        let heavy = if heavy.is_empty() { "None yet".to_string() } else { heavy.join(", ") };
        let rows = [("LEARNING FOR", learning, TEXT), ("MAP", format!("{known} of {} known", smart::LOADS * smart::STEPS), TEXT), ("COOLING NOW", cooling, ccol), ("HEAVY PROGRAMS", heavy, TEXT)];
        for (k, (label, value, col)) in rows.iter().enumerate() {
            field(p, pos2(sx, top - 4.0 + k as f32 * 44.0), label, value, *col);
        }

        let by = ed.bottom() - 30.0;
        status_line(p, pos2(ed.left() + 18.0, by), "Always learning: it follows dust, room temperature and how you use the laptop", DIM);
        let forget = Rect::from_min_size(pos2(ed.right() - 18.0 - 96.0, by - 17.0), vec2(96.0, 34.0));
        if button(ui, forget, "smart_forget", "Forget", false, bv.learned_secs > 0.0 || !bv.apps.is_empty()) {
            self.send(Cmd::SmartForget);
        }
    }

    fn toggle_autostart(&mut self) {
        let want = !self.autostart;
        let result = if want {
            std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| autostart::enable(&exe))
        } else {
            autostart::disable()
        };
        let msg = match result {
            Ok(()) => {
                self.autostart = want;
                (if want { "Will start with Windows" } else { "Won't start with Windows" }.to_string(), true)
            }
            Err(e) => (format!("Start with Windows failed: {e}"), false),
        };
        self.shared.lock().unwrap().message = Some((msg.0, msg.1, Instant::now()));
    }
}

// ----------------------------------------------------------------- drawing

fn card(p: &Painter, r: Rect) {
    p.rect_filled(r, RADIUS, CARD);
    // the card under the pointer lights up a little
    let ctx = p.ctx();
    let over = ctx.pointer_hover_pos().is_some_and(|m| r.contains(m));
    let h = ctx.animate_bool_with_time(Id::new(("card", r.left() as i32, r.width() as i32, r.height() as i32)), over, 0.2);
    if h > 0.0 {
        vgradient(p, Rect::from_min_max(r.min + vec2(RADIUS, 1.0), pos2(r.right() - RADIUS, r.top() + 46.0)), Color32::from_white_alpha((h * 7.0) as u8), Color32::TRANSPARENT);
    }
    p.rect_stroke(r, RADIUS, Stroke::new(1.0, mix(BORDER, Color32::from_rgb(62, 66, 80), h)), StrokeKind::Inside);
}

fn card_title(p: &Painter, r: Rect, title: &str) {
    p.text(r.left_top() + vec2(18.0, 16.0), Align2::LEFT_TOP, title, semibold(15.0), TEXT);
}

/// Vertical gradient rectangle (no rounding).
fn vgradient(p: &Painter, r: Rect, top: Color32, bottom: Color32) {
    let mut m = Mesh::default();
    m.colored_vertex(r.left_top(), top);
    m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.left_bottom(), bottom);
    m.colored_vertex(r.right_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 3, 2);
    p.add(Shape::mesh(m));
}

/// Anti-aliased thick arc from angle a0 to a1 (radians, y-down), colour per angle.
fn arc(p: &Painter, c: Pos2, r: f32, w: f32, a0: f32, a1: f32, color: impl Fn(f32) -> Color32) {
    let sweep = a1 - a0;
    if sweep.abs() < 1e-3 {
        return;
    }
    let n = ((sweep.abs().to_degrees() / 2.0).ceil() as usize).max(2);
    let radii = [r - w / 2.0 - 1.0, r - w / 2.0, r + w / 2.0, r + w / 2.0 + 1.0];
    let mut m = Mesh::default();
    for i in 0..=n {
        let a = a0 + sweep * i as f32 / n as f32;
        let d = vec2(a.cos(), a.sin());
        let col = color(a);
        for (k, rad) in radii.iter().enumerate() {
            let cc = if k == 0 || k == 3 { Color32::TRANSPARENT } else { col };
            m.colored_vertex(c + d * *rad, cc);
        }
        if i > 0 {
            let b = (4 * i) as u32;
            let pb = b - 4;
            for k in 0..3 {
                m.add_triangle(pb + k, pb + k + 1, b + k);
                m.add_triangle(pb + k + 1, b + k + 1, b + k);
            }
        }
    }
    p.add(Shape::mesh(m));
}

const A0: f32 = 0.75 * PI; // 135°, bottom-left
const SWEEP: f32 = 1.5 * PI; // 270°

/// Eases a live reading toward `target` (time constant `tau` seconds) without
/// asking for repaints. egui's own animate_* helpers redraw at the screen's
/// full refresh rate while a value is changing; live readings change every
/// second, so on a 144 Hz screen that kept one CPU core busy. Frames come
/// from the Dashboard's 60 fps pacing (or the once-a-second data tick).
fn ease(ctx: &egui::Context, id: Id, target: f32, tau: f32) -> f32 {
    let dt = ctx.input(|i| i.stable_dt).min(0.25);
    ctx.data_mut(|d| {
        let v = d.get_temp_mut_or(id, target);
        *v += (target - *v) * (1.0 - (-dt / tau).exp());
        if (target - *v).abs() < 0.02 {
            *v = target;
        }
        *v
    })
}

/// Temperature gauge sized to its column. `now` (seconds) drives the
/// animation: a pulse ring on the arc's tip (faster and wider when hot) and
/// a shine that runs along the filled arc. `target` is the fan speed this
/// sensor's curve asks for, shown underneath.
#[allow(clippy::too_many_arguments)]
fn gauge(ui: &Ui, r: Rect, id: Id, name: &str, accent: Color32, temp: u8, target: u8, now: f64) {
    let p = ui.painter();
    let shown = ease(ui.ctx(), id, temp as f32, 0.25);
    let target_shown = ease(ui.ctx(), id.with("target"), target as f32, 0.25);
    let f = temp_frac(shown);

    let rad = ((r.width() - 40.0) / 2.0).min(70.0);
    let k = rad / 70.0; // scale fonts and offsets with the gauge
    let w = 11.0 * k;
    let c = pos2(r.center().x, r.top() + rad + 18.0);
    let at = |a: f32| (a - A0) / SWEEP;

    arc(p, c, rad, w, A0, A0 + SWEEP, |_| TRACK);
    if f > 0.0 {
        let a1 = A0 + SWEEP * f;
        // glow under the arc, breathing a little when hot
        let hot = ((shown - 75.0) / 20.0).clamp(0.0, 1.0);
        let breathe = 0.07 + hot * 0.06 * (0.5 + 0.5 * (now as f32 * 3.0).sin());
        arc(p, c, rad, w * 1.9, A0, a1, |a| heat(at(a)).gamma_multiply(breathe));
        arc(p, c, rad, w, A0, a1, |a| heat(at(a)));
        // shine: a soft white highlight sliding from the start to the tip
        let span = a1 - A0;
        let pos = A0 + span * (now as f32 / 2.6).fract();
        let half = 0.32;
        let (s0, s1) = ((pos - half).max(A0), (pos + half).min(a1));
        if s1 > s0 {
            arc(p, c, rad, w * 0.55, s0, s1, |a| {
                let d = 1.0 - ((a - pos) / half).abs();
                Color32::from_white_alpha((d * d * 70.0) as u8)
            });
        }
        let cap = |a: f32| c + vec2(a.cos(), a.sin()) * rad;
        p.circle_filled(cap(A0), w / 2.0, heat(0.0));
        // tip: an expanding, fading pulse ring, faster when hot
        let rate = 0.55 + 1.1 * hot;
        let ph = (now as f32 * rate).fract();
        let tip = cap(a1);
        p.circle_stroke(tip, w / 2.0 + 2.5 + ph * 9.0 * k, Stroke::new(2.0, heat(f).gamma_multiply((1.0 - ph) * 0.55)));
        p.circle_filled(tip, w / 2.0 + 2.5, heat(f));
        p.circle_filled(tip, 2.5, CARD);
    }

    // scale labels
    let lbl = |a: f32| c + vec2(a.cos(), a.sin()) * (rad + 4.0) + vec2(0.0, 16.0 * k);
    p.text(lbl(A0), Align2::CENTER_CENTER, "30°", regular(11.0), FAINT);
    p.text(lbl(A0 + SWEEP), Align2::CENTER_CENTER, "100°", regular(11.0), FAINT);

    // centre
    p.circle_filled(c + vec2(-16.0, -34.0 * k), 3.5, accent);
    p.text(c + vec2(-8.0, -34.0 * k), Align2::LEFT_CENTER, name, semibold(13.0), DIM);
    let num = p.layout_no_wrap(format!("{}", shown.round() as i32), bold(44.0 * k), TEXT);
    let unit = p.layout_no_wrap("°C".into(), semibold(15.0 * k.max(0.85)), DIM);
    let total = num.size().x + 2.0 + unit.size().x;
    let x0 = c.x - total / 2.0;
    let ny = c.y - num.size().y / 2.0 + 2.0;
    let num_w = num.size().x;
    p.galley(pos2(x0, ny), num, TEXT);
    p.galley(pos2(x0 + num_w + 2.0, ny + 9.0 * k), unit, DIM);
    let word = match shown {
        t if t < 60.0 => "Cool",
        t if t < 80.0 => "Warm",
        _ => "Hot",
    };
    p.text(c + vec2(0.0, 30.0 * k), Align2::CENTER_CENTER, word, semibold(12.0), heat(f));

    // what this sensor's curve asks the (one) fan for
    let y = c.y + rad + 34.0;
    let bar = Rect::from_min_max(pos2(r.left() + 22.0, y + 18.0), pos2(r.right() - 22.0, y + 23.0));
    p.text(pos2(bar.left(), y), Align2::LEFT_TOP, "FAN TARGET", semibold(10.5), FAINT);
    p.text(pos2(bar.right(), y), Align2::RIGHT_TOP, format!("{}%", target_shown.round() as i32), semibold(12.0), TEXT);
    p.rect_filled(bar, 2.5, TRACK);
    let fw = bar.width() * (target_shown / 100.0).clamp(0.0, 1.0);
    if fw > 1.0 {
        let fill = Rect::from_min_size(bar.min, vec2(fw.max(5.0), bar.height()));
        p.rect_filled(fill.expand(2.0), 4.0, accent.gamma_multiply(0.15));
        p.rect_filled(fill, 2.5, accent);
    }
}

/// Highest fan speed seen on this laptop (Cooler Boost), for the speed ring.
const MAX_RPM: f32 = 6900.0;
/// The on-screen fan turns exactly this many times slower than the real one.
const SLOW_MO: f32 = 60.0;

/// The laptop's one fan: a housing, seven curved blades turned by `angle`,
/// motion blur from `spin` (radians per frame), and a speed ring around it
/// that fills and warms with the RPM. Boost turns the ring red and pulsing.
#[allow(clippy::too_many_arguments)]
fn fan_art(p: &Painter, c: Pos2, rad: f32, angle: f32, spin: f32, rpm: f32, boost: bool, now: f64) {
    use std::f32::consts::TAU;
    let speed = (rpm / MAX_RPM).clamp(0.0, 1.0);

    // speed ring: track, glow and fill from 12 o'clock, blue -> red
    let ring_r = rad + 10.0;
    let a_top = -PI / 2.0;
    arc(p, c, ring_r, 4.0, 0.0, TAU, |_| TRACK);
    if speed > 0.0 {
        let a1 = a_top + TAU * speed;
        let col = |a: f32| {
            let t = ((a - a_top) / TAU).clamp(0.0, 1.0);
            if boost { ACCENT } else { mix(GPU_C, HOT, t) }
        };
        let glow = if boost { 0.16 + 0.10 * (now as f32 * 4.0).sin() } else { 0.12 };
        arc(p, c, ring_r, 13.0, a_top, a1, |a| col(a).gamma_multiply(glow));
        arc(p, c, ring_r, 4.0, a_top, a1, col);
        p.circle_filled(c + vec2(a1.cos(), a1.sin()) * ring_r, 3.5, col(a1));
    }

    // housing
    p.circle_filled(c, rad + 3.0, Color32::from_rgb(12, 13, 18));
    p.circle_stroke(c, rad + 3.0, Stroke::new(1.5, Color32::from_white_alpha(14)));
    p.circle_stroke(c, rad - 1.0, Stroke::new(1.0, Color32::from_black_alpha(120)));

    // blades: ribbons from hub to tip, wider and swept forward at the tip
    let hub = rad * 0.27;
    let tip = rad * 0.92;
    let blades = 7;
    let blade = |p: &Painter, base: f32, alpha: f32| {
        let mut m = Mesh::default();
        let steps = 12;
        let mut edge = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let r = hub + (tip - hub) * t;
            let lead = base + 0.55 * t.powf(1.25);
            let trail = lead - (0.30 + 0.22 * t);
            let shade = mix(Color32::from_rgb(52, 56, 68), Color32::from_rgb(104, 110, 126), t);
            let col = shade.gamma_multiply(alpha);
            m.colored_vertex(c + vec2(lead.cos(), lead.sin()) * r, col);
            m.colored_vertex(c + vec2(trail.cos(), trail.sin()) * r, col.gamma_multiply(0.75));
            edge.push(c + vec2(lead.cos(), lead.sin()) * r);
            if i > 0 {
                let b = (2 * i) as u32;
                m.add_triangle(b - 2, b - 1, b);
                m.add_triangle(b - 1, b + 1, b);
            }
        }
        p.add(Shape::mesh(m));
        // a thin highlight on the leading edge
        p.add(Shape::line(edge, Stroke::new(1.2, Color32::from_white_alpha((45.0 * alpha) as u8))));
    };
    // motion blur: fading copies trailing behind, then the blades themselves
    let ghosts = if spin > 0.01 { 3 } else { 0 };
    for g in (1..=ghosts).rev() {
        let back = spin * g as f32 / ghosts as f32;
        for k in 0..blades {
            blade(p, angle - back + k as f32 * TAU / blades as f32, 0.22 / g as f32);
        }
    }
    let solid = 1.0 - 0.35 * speed;
    for k in 0..blades {
        blade(p, angle + k as f32 * TAU / blades as f32, solid);
    }
    // at speed the blades blur into a faint disc
    p.circle_filled(c, tip, Color32::from_white_alpha((10.0 * speed) as u8));

    // hub: dark cap, red core (like the logo), small highlight
    p.circle_filled(c, hub, Color32::from_rgb(28, 31, 39));
    p.circle_stroke(c, hub, Stroke::new(1.5, Color32::from_white_alpha(28)));
    p.circle_filled(c, hub * 0.46, if boost { ACCENT } else { ACCENT.gamma_multiply(0.85) });
    p.circle_filled(c + vec2(-hub * 0.32, -hub * 0.32), hub * 0.22, Color32::from_white_alpha(22));
}

fn chart(p: &Painter, r: Rect, history: &VecDeque<(u8, u8)>) {
    card_title(p, r, "Temperature");
    p.text(r.left_top() + vec2(118.0, 18.0), Align2::LEFT_TOP, "last 2 min", regular(12.0), FAINT);
    let mut lx = r.right() - 18.0;
    for (name, col) in [("GPU", GPU_C), ("CPU", CPU_C)] {
        let g = p.layout_no_wrap(name.into(), semibold(12.0), DIM);
        lx -= g.size().x;
        p.galley(pos2(lx, r.top() + 17.0), g, DIM);
        p.circle_filled(pos2(lx - 9.0, r.top() + 25.0), 4.0, col);
        lx -= 26.0;
    }

    let plot = Rect::from_min_max(r.left_top() + vec2(18.0, 46.0), r.right_bottom() - vec2(44.0, 14.0));
    let y = |t: f32| plot.bottom() - temp_frac(t) * plot.height();
    for t in [40.0, 60.0, 80.0] {
        let yy = y(t);
        let mut x = plot.left();
        while x < plot.right() {
            p.line_segment([pos2(x, yy), pos2((x + 4.0).min(plot.right()), yy)], Stroke::new(1.0, BORDER));
            x += 8.0;
        }
        p.text(pos2(plot.right() + 8.0, yy), Align2::LEFT_CENTER, format!("{t:.0}°"), regular(11.0), FAINT);
    }
    if history.len() < 2 {
        p.text(plot.center(), Align2::CENTER_CENTER, "collecting data...", regular(12.0), FAINT);
        return;
    }
    let step = plot.width() / (HISTORY - 1) as f32;
    let x0 = plot.right() - step * (history.len() - 1) as f32;
    for (idx, col) in [(1usize, GPU_C), (0, CPU_C)] {
        let pts: Vec<Pos2> = history
            .iter()
            .enumerate()
            .map(|(i, &(c, g))| pos2(x0 + i as f32 * step, y(if idx == 0 { c } else { g } as f32)))
            .collect();
        // area fill
        let mut m = Mesh::default();
        for (i, pt) in pts.iter().enumerate() {
            m.colored_vertex(*pt, col.gamma_multiply(if idx == 0 { 0.16 } else { 0.10 }));
            m.colored_vertex(pos2(pt.x, plot.bottom()), Color32::TRANSPARENT);
            if i > 0 {
                let b = (2 * i) as u32;
                m.add_triangle(b - 2, b - 1, b);
                m.add_triangle(b - 1, b + 1, b);
            }
        }
        p.add(Shape::mesh(m));
        let last = *pts.last().unwrap();
        p.add(Shape::line(pts, Stroke::new(2.0, col)));
        p.circle_filled(last, 8.0, col.gamma_multiply(0.2));
        p.circle_filled(last, 4.0, col);
    }
}

/// Segmented control with a sliding highlight. Returns the clicked index.
/// How long a click is shown as already applied while the laptop catches up
/// (the write, then the next read-back). After that the real state wins.
const PENDING_SECS: f64 = 1.5;

/// Instant feedback: remembers what was just clicked on control `id` and
/// returns it in place of `actual` until the hardware reports it (or the
/// grace time runs out, e.g. when the write was refused).
fn optimistic<T: Copy + PartialEq + Send + Sync + 'static>(ctx: &egui::Context, id: Id, actual: T, clicked: Option<T>) -> T {
    let key = id.with("pending");
    let now = ctx.input(|i| i.time);
    if let Some(v) = clicked {
        ctx.data_mut(|d| d.insert_temp(key, (v, now)));
    }
    match ctx.data(|d| d.get_temp::<(T, f64)>(key)) {
        Some((v, at)) if v != actual && now - at < PENDING_SECS => {
            ctx.request_repaint_after(Duration::from_millis(200));
            v
        }
        Some(_) => {
            ctx.data_mut(|d| d.remove::<(T, f64)>(key));
            actual
        }
        None => actual,
    }
}

fn segmented(ui: &mut Ui, r: Rect, id: Id, opts: &[&str], sel: Option<usize>, enabled: bool) -> Option<usize> {
    let p = ui.painter().clone();
    p.rect_filled(r, 10.0, WELL);
    p.rect_stroke(r, 10.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let seg = r.width() / opts.len() as f32;
    // show a click at once; the laptop confirms within a second
    let sel = optimistic(ui.ctx(), id, sel, None);
    if let Some(s) = sel {
        let x = ui.ctx().animate_value_with_time(id, s as f32, 0.18);
        let pill = Rect::from_min_size(pos2(r.left() + x * seg, r.top()), vec2(seg, r.height())).shrink(3.0);
        let col = if enabled { ACCENT } else { ACCENT.gamma_multiply(0.4) };
        p.rect_filled(pill.expand(3.0), 10.0, col.gamma_multiply(0.18));
        p.rect_filled(pill, 8.0, col);
    }
    let mut clicked = None;
    for (i, label) in opts.iter().enumerate() {
        let sr = Rect::from_min_size(pos2(r.left() + i as f32 * seg, r.top()), vec2(seg, r.height()));
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let mut resp = ui.interact(sr, id.with(i), sense);
        let selected = sel == Some(i);
        if enabled && !selected {
            // hover fades in; pressing pushes the cell in a little
            let h = ui.ctx().animate_bool_with_time(id.with(("hover", i)), resp.hovered(), 0.12);
            let press = if resp.is_pointer_button_down_on() { 2.0 } else { 0.0 };
            if h > 0.0 {
                p.rect_filled(sr.shrink(3.0 + press), 8.0, Color32::from_white_alpha((h * 9.0) as u8));
            }
        }
        let mut col = if selected {
            Color32::WHITE
        } else if resp.hovered() && enabled {
            TEXT
        } else {
            DIM
        };
        if !enabled {
            col = col.gamma_multiply(0.45);
        }
        p.text(sr.center(), Align2::CENTER_CENTER, label, semibold(14.0), col);
        if enabled {
            resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    if let Some(i) = clicked {
        optimistic(ui.ctx(), id, sel, Some(Some(i)));
    }
    clicked
}

/// Animated on/off switch. Returns true when clicked.
fn switch(ui: &mut Ui, r: Rect, id: Id, on: bool, enabled: bool) -> bool {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let mut resp = ui.interact(r, id, sense);
    // flips at once on a click; the laptop confirms within a second
    let on = optimistic(ui.ctx(), id, on, resp.clicked().then_some(!on));
    let t = ui.ctx().animate_bool_with_time(id, on, 0.18);
    let p = ui.painter();
    let mut track = mix(TRACK, ACCENT, t);
    if !enabled {
        track = track.gamma_multiply(0.45);
    }
    if t > 0.0 {
        p.rect_filled(r.expand(3.0), r.height() / 2.0 + 3.0, ACCENT.gamma_multiply(0.15 * t));
    }
    p.rect_filled(r, r.height() / 2.0, track);
    let kr = r.height() / 2.0 - 4.0 + if resp.hovered() && enabled { 1.0 } else { 0.0 };
    let kx = egui::lerp((r.left() + r.height() / 2.0)..=(r.right() - r.height() / 2.0), t);
    p.circle_filled(pos2(kx, r.center().y), kr, Color32::WHITE);
    if enabled {
        resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// The layout is designed for WIN_W x WIN_H. Maximised, scale the whole UI
/// so that size fills the screen (like zooming in) instead of stretching
/// the cards; back to 1x when restored.
fn fit_zoom(root: &Ui) {
    let ctx = root.ctx();
    let maxed = win::is_maximised() || ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let want = if maxed {
        let native = ctx.native_pixels_per_point().unwrap_or(1.0);
        let logical = root.max_rect().size() * ctx.pixels_per_point() / native;
        (logical.x / WIN_W).min(logical.y / WIN_H).clamp(1.0, 3.0)
    } else {
        1.0
    };
    if (ctx.zoom_factor() - want).abs() > 0.005 {
        ctx.set_zoom_factor(want);
        ctx.request_repaint();
    }
    if let Some(path) = std::env::var_os("MSI_SELF_SHOT") {
        self_shot(ctx, std::path::Path::new(&path));
    }
}

/// Debug aid (MSI_SELF_SHOT=<dir>): every ~2 s, saves the app's own rendered
/// frame as a BMP. Screen capture can't see a full-screen GL window's frames.
fn self_shot(ctx: &egui::Context, dir: &std::path::Path) {
    for img in ctx.input(|i| {
        i.raw.events.iter().filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }).collect::<Vec<_>>()
    }) {
        let (w, h) = (img.size[0] as u32, img.size[1] as u32);
        let mut bmp = Vec::with_capacity(54 + (w * h * 4) as usize);
        bmp.extend_from_slice(b"BM");
        for v in [54 + w * h * 4, 0, 54, 40] {
            bmp.extend_from_slice(&v.to_le_bytes());
        }
        bmp.extend_from_slice(&(w as i32).to_le_bytes());
        bmp.extend_from_slice(&(-(h as i32)).to_le_bytes());
        bmp.extend_from_slice(&[1, 0, 32, 0]);
        bmp.extend_from_slice(&[0u8; 24]);
        for p in &img.pixels {
            bmp.extend_from_slice(&[p.b(), p.g(), p.r(), 255]);
        }
        let n = ctx.input(|i| i.time) as u64;
        let _ = std::fs::write(dir.join(format!("self-{n:04}.bmp")), bmp);
    }
    let t = ctx.input(|i| i.time);
    let slot = (t / 2.0) as u64;
    let last = ctx.data_mut(|d| std::mem::replace(d.get_temp_mut_or_default::<u64>(egui::Id::new("shot_slot")), slot));
    if last != slot {
        ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(500));
}

/// The logo as a GPU texture, made once (256 px with mipmaps: crisp at
/// any zoom).
fn logo_texture(ctx: &egui::Context) -> egui::TextureId {
    let key = Id::new("logo_texture");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return t.id();
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([256, 256], &dragonslayer::icon::rgba(256));
    let opts = egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
    let t = ctx.load_texture("logo", img, opts);
    let id = t.id();
    ctx.data_mut(|d| d.insert_temp(key, t));
    id
}

/// Custom title bar with the tabs. Returns its rect.
fn title_bar(ui: &mut Ui, full: Rect, mock: bool, tab: &mut Tab) -> Rect {
    let tb = Rect::from_min_size(full.min + vec2(PAD, 8.0), vec2(full.width() - 2.0 * PAD, 40.0));
    let os_maxed = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
    let maxed = os_maxed || win::is_maximised();
    let toggle_max = |ui: &Ui| {
        if os_maxed {
            ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(false));
        } else {
            win::set_maximised(!maxed);
        }
    };
    let drag = ui.interact(tb, Id::new("titlebar"), Sense::click_and_drag());
    if drag.drag_started() {
        win::restore_under_cursor();
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        toggle_max(ui);
    }
    let p = ui.painter().clone();

    // logo (the app icon), with a soft red glow behind it
    let logo = Rect::from_min_size(pos2(tb.left(), tb.center().y - 15.0), vec2(30.0, 30.0));
    p.rect_filled(logo.expand(2.0), 10.0, ACCENT.gamma_multiply(0.14));
    p.image(logo_texture(ui.ctx()), logo, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    p.text(pos2(logo.right() + 12.0, tb.center().y - 9.0), Align2::LEFT_CENTER, "DragonSlayer", bold(16.0), TEXT);
    p.text(pos2(logo.right() + 12.0, tb.center().y + 10.0), Align2::LEFT_CENTER, "GF63 Thin 10UC", regular(11.5), FAINT);

    // tabs, centred, with a sliding underline
    let tabs = [(Tab::Dashboard, "Dashboard"), (Tab::Fans, "Fan"), (Tab::Tools, "Tools"), (Tab::System, "System"), (Tab::Automation, "Automation")];
    let tw = 100.0;
    let tx0 = tb.center().x - tw * tabs.len() as f32 / 2.0;
    for (i, (t, label)) in tabs.iter().enumerate() {
        let r = Rect::from_min_size(pos2(tx0 + i as f32 * tw, tb.top()), vec2(tw, tb.height()));
        let resp = ui.interact(r, Id::new(("tab", i)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let sel = *tab == *t;
        if resp.hovered() && !sel {
            p.rect_filled(r.shrink2(vec2(8.0, 5.0)), 8.0, Color32::from_white_alpha(6));
        }
        let col = if sel { TEXT } else if resp.hovered() { mix(DIM, TEXT, 0.5) } else { DIM };
        p.text(r.center() - vec2(0.0, 2.0), Align2::CENTER_CENTER, label, semibold(14.0), col);
        if resp.clicked() {
            *tab = *t;
        }
    }
    let idx = tabs.iter().position(|(t, _)| t == tab).unwrap_or(0) as f32;
    let x = ui.ctx().animate_value_with_time(Id::new("tab_line"), idx, 0.22);
    let ul = Rect::from_min_size(pos2(tx0 + x * tw + tw / 2.0 - 16.0, tb.bottom() - 6.0), vec2(32.0, 3.0));
    p.rect_filled(ul.expand(2.0), 3.0, ACCENT.gamma_multiply(0.2));
    p.rect_filled(ul, 1.5, ACCENT);

    // window buttons
    let close = Rect::from_min_size(pos2(tb.right() - 34.0, tb.center().y - 15.0), vec2(34.0, 30.0));
    let maxim = close.translate(vec2(-38.0, 0.0));
    let minim = maxim.translate(vec2(-38.0, 0.0));
    let c_resp = ui.interact(close, Id::new("close"), Sense::click());
    let x_resp = ui.interact(maxim, Id::new("max"), Sense::click()).on_hover_text(if maxed { "Restore" } else { "Maximise" });
    let m_resp = ui.interact(minim, Id::new("min"), Sense::click());
    if c_resp.hovered() {
        p.rect_filled(close, 8.0, HOT);
    }
    if x_resp.hovered() {
        p.rect_filled(maxim, 8.0, BORDER);
    }
    if m_resp.hovered() {
        p.rect_filled(minim, 8.0, BORDER);
    }
    // maximise: one square; restore: two overlapping squares
    let xc = maxim.center();
    let xst = Stroke::new(1.4, if x_resp.hovered() { TEXT } else { DIM });
    if maxed {
        let front = Rect::from_center_size(xc + vec2(-1.5, 1.5), vec2(8.0, 8.0));
        p.rect_stroke(front, 1.5, xst, StrokeKind::Middle);
        let (a, b) = (front.left_top() + vec2(3.0, -3.0), front.right_bottom() + vec2(3.0, -3.0));
        p.line_segment([a, pos2(b.x, a.y)], xst);
        p.line_segment([pos2(b.x, a.y), b], xst);
    } else {
        p.rect_stroke(Rect::from_center_size(xc, vec2(10.0, 10.0)), 1.5, xst, StrokeKind::Middle);
    }
    if x_resp.clicked() {
        toggle_max(ui);
    }
    let cc = close.center();
    let xs = Stroke::new(1.6, if c_resp.hovered() { Color32::WHITE } else { DIM });
    p.line_segment([cc + vec2(-5.0, -5.0), cc + vec2(5.0, 5.0)], xs);
    p.line_segment([cc + vec2(-5.0, 5.0), cc + vec2(5.0, -5.0)], xs);
    let mc = minim.center();
    p.line_segment([mc + vec2(-5.0, 0.0), mc + vec2(5.0, 0.0)], Stroke::new(1.6, if m_resp.hovered() { TEXT } else { DIM }));
    if c_resp.clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
    }
    if m_resp.clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
    }

    // status pill
    let (txt, col) = if mock { ("DEMO", WARM) } else { ("LIVE", COOL) };
    let pill = Rect::from_min_size(pos2(minim.left() - 74.0, tb.center().y - 12.0), vec2(64.0, 24.0));
    p.rect_filled(pill, 12.0, col.gamma_multiply(0.12));
    p.circle_filled(pos2(pill.left() + 15.0, pill.center().y), 6.0, col.gamma_multiply(0.25));
    p.circle_filled(pos2(pill.left() + 15.0, pill.center().y), 3.5, col);
    p.text(pos2(pill.left() + 26.0, pill.center().y), Align2::LEFT_CENTER, txt, semibold(11.5), col);
    tb
}

fn connecting(p: &Painter, area: Rect, error: Option<&str>) {
    let r = Rect::from_min_size(area.min, vec2(area.width(), 270.0));
    card(p, r);
    let (title, col) = if error.is_some() { ("Can't reach the controller", HOT) } else { ("Connecting", TEXT) };
    p.text(r.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, title, semibold(17.0), col);
    let msg = error.unwrap_or("Connecting to the laptop's controller...");
    p.text(r.center() + vec2(0.0, 16.0), Align2::CENTER_CENTER, msg, regular(13.0), DIM);
}

/// 1234 -> "1,234".
fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Thin rounded progress bar.
fn bar(p: &Painter, r: Rect, frac: f32, col: Color32) {
    p.rect_filled(r, r.height() / 2.0, TRACK);
    let w = r.width() * frac.clamp(0.0, 1.0);
    if w > 0.5 {
        p.rect_filled(Rect::from_min_size(r.min, vec2(w.max(r.height()), r.height())), r.height() / 2.0, col);
    }
}

/// Small filled line chart of `data` (0..max) inside `r`.
fn sparkline(p: &Painter, r: Rect, data: &VecDeque<f32>, max: f32, col: Color32, slots: usize) {
    if data.len() < 2 {
        return;
    }
    let step = r.width() / (slots - 1) as f32;
    let x0 = r.right() - step * (data.len() - 1) as f32;
    let pts: Vec<Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, v)| pos2(x0 + i as f32 * step, r.bottom() - (v / max).clamp(0.0, 1.0) * r.height()))
        .collect();
    let mut m = Mesh::default();
    for (i, pt) in pts.iter().enumerate() {
        m.colored_vertex(*pt, col.gamma_multiply(0.18));
        m.colored_vertex(pos2(pt.x, r.bottom()), Color32::TRANSPARENT);
        if i > 0 {
            let b = (2 * i) as u32;
            m.add_triangle(b - 2, b - 1, b);
            m.add_triangle(b - 1, b + 1, b);
        }
    }
    p.add(Shape::mesh(m));
    p.add(Shape::line(pts, Stroke::new(1.8, col)));
}

/// Big number + unit at the top-left of a stat card.
fn big_value(p: &Painter, r: Rect, value: String, unit: &str) {
    let g = p.layout_no_wrap(value, bold(34.0), TEXT);
    let (w, h) = (g.size().x, g.size().y);
    let pos = r.left_top() + vec2(18.0, 40.0);
    p.galley(pos, g, TEXT);
    p.text(pos + vec2(w + 4.0, h - 9.0), Align2::LEFT_BOTTOM, unit, semibold(15.0), DIM);
}

/// Label + value pair used in detail grids.
fn field(p: &Painter, pos: Pos2, label: &str, value: &str, col: Color32) {
    p.text(pos, Align2::LEFT_TOP, label, semibold(10.5), FAINT);
    p.text(pos + vec2(0.0, 17.0), Align2::LEFT_TOP, value, semibold(15.0), col);
}

/// Rounded push button. Returns true when clicked.
fn button(ui: &mut Ui, r: Rect, id: &str, label: &str, primary: bool, enabled: bool) -> bool {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let mut resp = ui.interact(r, Id::new(id), sense);
    let p = ui.painter();
    let hover = enabled && resp.hovered();
    let (fill, text) = match (primary, enabled) {
        (true, true) => (if hover { mix(ACCENT, Color32::WHITE, 0.12) } else { ACCENT }, Color32::WHITE),
        (true, false) => (ACCENT.gamma_multiply(0.3), Color32::WHITE.gamma_multiply(0.5)),
        (false, _) => (if hover { BORDER } else { WELL }, if enabled { TEXT } else { FAINT }),
    };
    if primary && enabled {
        p.rect_filled(r.expand(3.0), 11.0, ACCENT.gamma_multiply(0.16));
    }
    p.rect_filled(r, 9.0, fill);
    if !primary {
        p.rect_stroke(r, 9.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    }
    p.text(r.center(), Align2::CENTER_CENTER, label, semibold(13.5), text);
    if enabled {
        resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// Small status line: coloured dot + text.
fn status_line(p: &Painter, pos: Pos2, text: &str, col: Color32) {
    p.circle_filled(pos + vec2(4.0, 0.0), 3.5, col);
    p.text(pos + vec2(14.0, 0.0), Align2::LEFT_CENTER, text, semibold(12.5), col);
}

/// Which curve point is active for `temp` given the 6 step temperatures.
fn curve_step(temps: &[u8], temp: u8) -> usize {
    temps.iter().filter(|&&t| temp >= t).count()
}

/// Label + description on the left, switch on the right. Returns true on click.
#[allow(clippy::too_many_arguments)]
fn switch_row(ui: &mut Ui, p: &Painter, r: Rect, id: &str, title: &str, desc: &str, on: bool, enabled: bool) -> bool {
    p.text(pos2(r.left(), r.center().y - 9.0), Align2::LEFT_CENTER, title, semibold(14.0), TEXT);
    p.text(pos2(r.left(), r.center().y + 10.0), Align2::LEFT_CENTER, desc, regular(12.0), FAINT);
    let sw = Rect::from_min_size(pos2(r.right() - 52.0, r.center().y - 14.0), vec2(52.0, 28.0));
    switch(ui, sw, Id::new(id), on, enabled)
}

impl App {
    fn dashboard(&mut self, ui: &mut Ui, p: &Painter, area: Rect, st: &Status, history: &VecDeque<(u8, u8)>, can_write: bool) {
        // [ gauges  ][ temperature chart   ]
        // [ cooling ][ battery ][ keyboard ]
        let left_w = 500.0;
        let right_w = area.width() - left_w - GAP;
        let (x0, top) = (area.left(), area.top());
        let rx = x0 + left_w + GAP;
        let hero = Rect::from_min_size(pos2(x0, top), vec2(left_w, 270.0));
        let ch = Rect::from_min_size(pos2(rx, top), vec2(right_w, 270.0));
        let row2 = hero.bottom() + GAP;
        let cool = Rect::from_min_size(pos2(x0, row2), vec2(left_w, 130.0));
        let cw = (right_w - GAP) / 2.0;
        let bat = Rect::from_min_size(pos2(rx, row2), vec2(cw, 130.0));
        let kbd = Rect::from_min_size(pos2(rx + cw + GAP, row2), vec2(cw, 130.0));

        let (smart_on, smart_now) = {
            let s = self.shared.lock().unwrap();
            (s.settings.smart, s.smart.clone())
        };
        // [ CPU gauge ][ the one fan ][ GPU gauge ]
        card(p, hero);
        let now = ui.ctx().input(|i| i.time);
        let side = 165.0;
        let mid = hero.width() - 2.0 * side;
        let gtop = hero.min + vec2(0.0, 24.0);
        gauge(ui, Rect::from_min_size(gtop, vec2(side, 240.0)), Id::new("g_cpu"), "CPU", CPU_C, st.cpu_temp_c, st.cpu_fan_pct, now);
        gauge(ui, Rect::from_min_size(gtop + vec2(side + mid, 0.0), vec2(side, 240.0)), Id::new("g_gpu"), "GPU", GPU_C, st.gpu_temp_c, st.gpu_fan_pct, now);

        let rpm = ease(ui.ctx(), Id::new("rpm"), f32::from(st.fan_rpm), 0.35);
        // spin: locked to the tachometer, in exact SLOW_MO slow motion (one
        // turn on screen = 60 real turns). The real 40-115 turns a second
        // can't be shown on a 60 Hz screen: the blades would look frozen or
        // running backwards.
        let dt = ui.ctx().input(|i| i.unstable_dt).min(0.1);
        let spin = rpm / 60.0 / SLOW_MO * std::f32::consts::TAU * dt;
        self.fan_angle = (self.fan_angle + spin) % std::f32::consts::TAU;
        let fc = pos2(hero.left() + side + mid / 2.0, hero.top() + 106.0);
        fan_art(p, fc, 58.0, self.fan_angle, spin, rpm, st.cooler_boost, now);
        let rtxt = if st.fan_rpm == 0 { "stopped".to_string() } else { thousands(rpm.round() as u32) };
        let num = p.layout_no_wrap(rtxt, bold(24.0), TEXT);
        let unit = p.layout_no_wrap(if st.fan_rpm == 0 { String::new() } else { "RPM".into() }, semibold(11.5), DIM);
        let tw = num.size().x + 5.0 + unit.size().x;
        let ny = fc.y + 58.0 + 22.0;
        let nw = num.size().x;
        p.galley(pos2(fc.x - tw / 2.0, ny), num, TEXT);
        p.galley(pos2(fc.x - tw / 2.0 + nw + 5.0, ny + 11.0), unit, DIM);
        // mode chip
        let (mode, col) = if st.cooler_boost {
            ("COOLER BOOST".to_string(), ACCENT)
        } else if smart_on {
            (format!("SMART  ·  {}%", smart_now.as_ref().map_or(0, |d| d.fan_pct)), COOL)
        } else {
            (st.fan_mode.map_or("FAN", fan_name).to_uppercase(), DIM)
        };
        let g = p.layout_no_wrap(mode, semibold(10.5), col);
        let chip = Rect::from_center_size(pos2(fc.x, ny + 46.0), vec2(g.size().x + 22.0, 20.0));
        p.rect_filled(chip, 10.0, col.gamma_multiply(0.12));
        p.galley(chip.center() - g.size() / 2.0, g, col);
        if st.fan_rpm > 0 {
            p.text(pos2(fc.x, chip.bottom() + 13.0), Align2::CENTER_CENTER, "live  ·  60× slow motion", regular(10.5), FAINT);
        }

        // chart
        card(p, ch);
        chart(p, ch, history);

        // cooling
        card(p, cool);
        card_title(p, cool, "Cooling");
        let hint = match &smart_now {
            Some(d) => format!("Smart: {}", d.reason),
            None => "Auto: MSI's curve  ·  Custom: yours  ·  Smart: learns and decides itself".to_string(),
        };
        p.text(cool.left_bottom() + vec2(18.0, -20.0), Align2::LEFT_CENTER, hint, regular(12.0), if smart_now.is_some() { DIM } else { FAINT });
        let seg_r = Rect::from_min_size(cool.left_top() + vec2(18.0, 46.0), vec2(282.0, 40.0));
        self.fan_choice(ui, seg_r, "fan", st, smart_on, can_write);
        let sw = Rect::from_min_size(pos2(cool.right() - 18.0 - 52.0, seg_r.center().y - 14.0), vec2(52.0, 28.0));
        p.text(pos2(sw.left() - 14.0, seg_r.center().y - 9.0), Align2::RIGHT_CENTER, "Cooler Boost", semibold(14.0), TEXT);
        p.text(pos2(sw.left() - 14.0, seg_r.center().y + 10.0), Align2::RIGHT_CENTER, "Fan at full speed", regular(12.0), FAINT);
        if switch(ui, sw, Id::new("boost"), st.cooler_boost, can_write) {
            self.send(Cmd::Boost(!st.cooler_boost));
        }

        // battery limit + keyboard
        card(p, bat);
        card(p, kbd);
        card_title(p, bat, "Battery limit");
        let cur_charge = st.charge_limit.ok().map(|c| c.percent());
        let presets = [60u8, 80, 100];
        let bsel = cur_charge.and_then(|c| presets.iter().position(|&x| x == c));
        let bseg = Rect::from_min_size(bat.left_top() + vec2(14.0, 46.0), vec2(cw - 28.0, 38.0));
        if let Some(i) = segmented(ui, bseg, Id::new("bat"), &["60%", "80%", "100%"], bsel, can_write) {
            self.send(Cmd::Charge(presets[i]));
        }
        let bdesc = match (bsel, cur_charge) {
            (Some(0), _) => "Best for battery lifespan".to_string(),
            (Some(1), _) => "Balanced".to_string(),
            (Some(_), _) => "Full capacity".to_string(),
            (None, Some(c)) => format!("Stops charging at {c}%"),
            (None, None) => "Not set".to_string(),
        };
        p.text(pos2(bseg.left() + 4.0, bseg.bottom() + 12.0), Align2::LEFT_CENTER, bdesc, regular(12.0), FAINT);

        card_title(p, kbd, "Keyboard light");
        let ksel = st.kbd_backlight.ok().map(|k| k.level() as usize);
        let kseg = Rect::from_min_size(kbd.left_top() + vec2(14.0, 46.0), vec2(cw - 28.0, 38.0));
        if let Some(i) = segmented(ui, kseg, Id::new("kbd"), &["Off", "Low", "Mid", "High"], ksel, can_write) {
            self.send(Cmd::Kbd(i as u8));
        }
        let lvl = ksel.unwrap_or(0);
        for i in 0..3 {
            let c = pos2(kseg.left() + 8.0 + i as f32 * 14.0, kseg.bottom() + 12.0);
            p.circle_filled(c, 4.0, if i < lvl { ACCENT } else { TRACK });
        }
    }

    fn system(&mut self, p: &Painter, area: Rect, st: Option<&Status>, sys: &SysSnap) {
        let row_h = 140.0;
        let w4 = (area.width() - 3.0 * GAP) / 4.0;
        let cell = |i: usize| Rect::from_min_size(pos2(area.left() + i as f32 * (w4 + GAP), area.top()), vec2(w4, row_h));
        let dash = "—".to_string();

        // processor
        let c = cell(0);
        card(p, c);
        card_title(p, c, "Processor");
        big_value(p, c, sys.cpu_pct.map_or(dash.clone(), |v| format!("{v:.0}")), "%");
        sparkline(p, Rect::from_min_max(pos2(c.left() + 18.0, c.bottom() - 40.0), pos2(c.right() - 18.0, c.bottom() - 14.0)), &sys.cpu_hist, 100.0, CPU_C, 60);

        // memory
        let c = cell(1);
        card(p, c);
        card_title(p, c, "Memory");
        if let Some((used, total)) = sys.mem {
            big_value(p, c, format!("{used:.1}"), "GB");
            p.text(c.left_top() + vec2(18.0, 92.0), Align2::LEFT_TOP, format!("of {total:.0} GB  ·  {:.0}% used", 100.0 * used / total), regular(12.0), FAINT);
            bar(p, Rect::from_min_size(pos2(c.left() + 18.0, c.bottom() - 22.0), vec2(c.width() - 36.0, 6.0)), used / total, GPU_C);
        } else {
            big_value(p, c, dash.clone(), "");
        }

        // graphics
        let c = cell(2);
        card(p, c);
        card_title(p, c, "Graphics");
        if let Some(g) = &sys.gpu {
            big_value(p, c, format!("{:.0}", g.util_pct), "%");
            let watts = g.power_w.map_or(String::new(), |w| format!("  ·  {w:.0} W"));
            p.text(c.left_top() + vec2(18.0, 92.0), Align2::LEFT_TOP, format!("{:.0} MHz{watts}", g.clock_mhz), regular(12.0), FAINT);
            let name = g.name.replace("NVIDIA GeForce ", "");
            p.text(c.left_bottom() + vec2(18.0, -16.0), Align2::LEFT_CENTER, name, semibold(12.0), DIM);
        } else {
            big_value(p, c, dash.clone(), "");
            p.text(c.left_bottom() + vec2(18.0, -16.0), Align2::LEFT_CENTER, "nvidia-smi not available", regular(12.0), FAINT);
        }

        // fans
        let c = cell(3);
        card(p, c);
        card_title(p, c, "Fan");
        if let Some(st) = st {
            let mode = st.fan_mode.map_or("Unknown", fan_name);
            let mode = if st.cooler_boost { "Cooler Boost" } else { mode };
            p.text(c.right_top() + vec2(-18.0, 18.0), Align2::RIGHT_TOP, mode, semibold(12.0), if st.cooler_boost { ACCENT } else { DIM });
            for (i, (name, v, col)) in [("CPU", st.cpu_fan_pct, CPU_C), ("GPU", st.gpu_fan_pct, GPU_C)].into_iter().enumerate() {
                let y = c.top() + 54.0 + i as f32 * 40.0;
                p.text(pos2(c.left() + 18.0, y), Align2::LEFT_TOP, name, semibold(12.0), FAINT);
                p.text(pos2(c.right() - 18.0, y), Align2::RIGHT_TOP, format!("{v}%"), semibold(13.0), TEXT);
                bar(p, Rect::from_min_size(pos2(c.left() + 18.0, y + 20.0), vec2(c.width() - 36.0, 6.0)), v as f32 / 100.0, col);
            }
        }

        // battery health + details
        let top2 = area.top() + row_h + GAP;
        let bh = area.bottom() - top2;
        let health_r = Rect::from_min_size(pos2(area.left(), top2), vec2(330.0, bh));
        let det = Rect::from_min_max(pos2(health_r.right() + GAP, top2), pos2(area.right(), area.bottom()));
        card(p, health_r);
        card(p, det);
        card_title(p, health_r, "Battery health");
        card_title(p, det, "Battery details");

        let Some(b) = &sys.battery else {
            p.text(health_r.center(), Align2::CENTER_CENTER, "Reading battery...", regular(13.0), FAINT);
            return;
        };
        let health = b.health_pct().unwrap_or(0.0);
        let (col, verdict) = match health {
            h if h < 40.0 => (HOT, "Worn out"),
            h if h < 80.0 => (WARM, "Aging"),
            _ => (COOL, "Healthy"),
        };
        let c = pos2(health_r.center().x, health_r.top() + 128.0);
        let (rad, w) = (64.0, 11.0);
        arc(p, c, rad, w, A0, A0 + SWEEP, |_| TRACK);
        let f = (health / 100.0).clamp(0.0, 1.0);
        if f > 0.0 {
            arc(p, c, rad, w, A0, A0 + SWEEP * f, |_| col);
            let end = A0 + SWEEP * f;
            p.circle_filled(c + vec2(A0.cos(), A0.sin()) * rad, w / 2.0, col);
            p.circle_filled(c + vec2(end.cos(), end.sin()) * rad, w / 2.0, col);
        }
        p.text(c - vec2(0.0, 6.0), Align2::CENTER_CENTER, format!("{health:.0}%"), bold(38.0), TEXT);
        p.text(c + vec2(0.0, 24.0), Align2::CENTER_CENTER, "of original capacity", regular(11.5), FAINT);
        let vg = p.layout_no_wrap(verdict.into(), semibold(13.0), col);
        let pw = vg.size().x + 28.0;
        let pill = Rect::from_center_size(pos2(c.x, health_r.bottom() - 28.0), vec2(pw, 26.0));
        p.rect_filled(pill, 13.0, col.gamma_multiply(0.14));
        p.galley(pill.center() - vg.size() / 2.0, vg, col);

        // details grid: 3 columns x 2 rows
        let wh = |mwh: u32| format!("{:.1} Wh", mwh as f32 / 1000.0);
        let (ac, pct) = sys.power.unwrap_or((true, 0));
        let rate = match b.rate_mw {
            0 => "Not charging".to_string(),
            r if r > 0 => format!("Charging {:.1} W", r as f32 / 1000.0),
            r => format!("Draining {:.1} W", -r as f32 / 1000.0),
        };
        let fields = [
            ("DESIGN CAPACITY", wh(b.design_mwh)),
            ("FULL CHARGE NOW", wh(b.full_mwh)),
            ("CHARGE", format!("{pct}%  ·  {}", if ac { "plugged in" } else { "on battery" })),
            ("CHARGING", rate),
            ("VOLTAGE", format!("{:.2} V", b.voltage_mv as f32 / 1000.0)),
            ("CHARGE CYCLES", if b.cycles == 0 { "Not reported".into() } else { b.cycles.to_string() }),
        ];
        let colw = (det.width() - 36.0) / 3.0;
        for (i, (k, v)) in fields.iter().enumerate() {
            let pos = det.left_top() + vec2(18.0 + (i % 3) as f32 * colw, 50.0 + (i / 3) as f32 * 52.0);
            let vcol = if i == 1 && health < 40.0 { HOT } else { TEXT };
            field(p, pos, k, v, vcol);
        }

        // advice
        let adv = Rect::from_min_max(pos2(det.left() + 18.0, det.bottom() - 74.0), pos2(det.right() - 18.0, det.bottom() - 16.0));
        p.rect_filled(adv, 10.0, col.gamma_multiply(0.10));
        p.rect_filled(Rect::from_min_size(adv.min, vec2(4.0, adv.height())), 2.0, col);
        let (l1, l2) = match health {
            h if h < 40.0 => (
                "This battery is worn out and can't run the laptop without the charger.",
                "A replacement fixes it (GF63 Thin uses MSI BTY-M6K, 51 Wh) - check your battery's label first.",
            ),
            h if h < 80.0 => ("The battery is aging.", "Keep the 80% limit on to slow further wear."),
            _ => ("The battery is in good shape.", "An 80% limit helps keep it that way if you're usually plugged in."),
        };
        p.text(adv.left_top() + vec2(16.0, 12.0), Align2::LEFT_TOP, l1, semibold(13.0), TEXT);
        p.text(adv.left_top() + vec2(16.0, 32.0), Align2::LEFT_TOP, l2, regular(12.0), DIM);
    }

    fn fans(&mut self, ui: &mut Ui, p: &Painter, area: Rect, st: &Status, s: &Settings, can_write: bool) {
        // [ curve editor            ][ fan mode ]
        // [                         ][ right now ]
        let ed = Rect::from_min_size(area.min, vec2(680.0, area.height()));
        let side_x = ed.right() + GAP;
        let side_w = area.right() - side_x;
        let mode_r = Rect::from_min_size(pos2(side_x, area.top()), vec2(side_w, 180.0));
        let now_r = Rect::from_min_max(pos2(side_x, mode_r.bottom() + GAP), area.max);
        for r in [ed, mode_r, now_r] {
            card(p, r);
        }

        let cpu_step = curve_step(&st.cpu_curve_temps, st.cpu_temp_c);
        let gpu_step = curve_step(&st.gpu_curve_temps, st.gpu_temp_c);
        if s.smart {
            self.smart_panel(ui, p, ed, s, can_write);
        } else {
        // ---- editor
        card_title(p, ed, "Fan curve");
        p.text(ed.left_top() + vec2(18.0, 38.0), Align2::LEFT_TOP, "Drag the bars  ·  CPU + GPU  ·  grey line = lowest allowed", regular(12.0), FAINT);
        let saved = s.curve.unwrap_or(FanCurve::FACTORY).speeds();
        let mut draft = self.draft.unwrap_or(saved);

        let presets = [("Quiet", FanCurve::QUIET), ("Factory", FanCurve::FACTORY), ("Cool", FanCurve::COOL), ("Max", FanCurve::MAX)];
        let psel = presets.iter().position(|(_, c)| c.speeds() == draft);
        let pr = Rect::from_min_size(pos2(ed.right() - 18.0 - 300.0, ed.top() + 14.0), vec2(300.0, 34.0));
        let names: Vec<&str> = presets.iter().map(|(n, _)| *n).collect();
        if let Some(i) = segmented(ui, pr, Id::new("presets"), &names, psel, true) {
            draft = presets[i].1.speeds();
        }

        let plot = Rect::from_min_max(pos2(ed.left() + 56.0, ed.top() + 76.0), pos2(ed.right() - 18.0, ed.bottom() - 92.0));
        let y = |v: f32| plot.bottom() - v / 100.0 * plot.height();
        for v in [25.0, 50.0, 75.0, 100.0] {
            let yy = y(v);
            let mut x = plot.left();
            while x < plot.right() {
                p.line_segment([pos2(x, yy), pos2((x + 4.0).min(plot.right()), yy)], Stroke::new(1.0, BORDER));
                x += 8.0;
            }
            p.text(pos2(plot.left() - 10.0, yy), Align2::RIGHT_CENTER, format!("{v:.0}%"), regular(11.0), FAINT);
        }
        p.line_segment([plot.left_bottom(), plot.right_bottom()], Stroke::new(1.0, BORDER));

        let colw = plot.width() / CURVE_POINTS as f32;
        let bw = colw * 0.52;
        let cpu_step = curve_step(&st.cpu_curve_temps, st.cpu_temp_c);
        let gpu_step = curve_step(&st.gpu_curve_temps, st.gpu_temp_c);
        let step_label = |temps: &[u8], i: usize| if i == 0 { format!("<{}°", temps[0]) } else { format!("{}°", temps[i - 1]) };
        p.text(pos2(plot.left() - 10.0, plot.bottom() + 16.0), Align2::RIGHT_CENTER, "CPU", semibold(10.5), CPU_C);
        p.text(pos2(plot.left() - 10.0, plot.bottom() + 33.0), Align2::RIGHT_CENTER, "GPU", semibold(10.5), GPU_C);
        let mut edited = None;
        #[allow(clippy::needless_range_loop)] // `i` also positions the column
        for i in 0..CURVE_POINTS {
            let col_r = Rect::from_min_max(pos2(plot.left() + colw * i as f32, plot.top() - 20.0), pos2(plot.left() + colw * (i + 1) as f32, plot.bottom()));
            let cx = col_r.center().x;
            // where the fan is right now
            if i == cpu_step || i == gpu_step {
                let tint = match (i == cpu_step, i == gpu_step) {
                    (true, true) => mix(CPU_C, GPU_C, 0.5),
                    (true, false) => CPU_C,
                    _ => GPU_C,
                };
                p.rect_filled(col_r.shrink2(vec2(4.0, 0.0)), 8.0, tint.gamma_multiply(0.07));
            }
            let resp = ui.interact(col_r, Id::new(("curve", i)), Sense::click_and_drag()).on_hover_cursor(CursorIcon::ResizeVertical);
            if (resp.dragged() || resp.is_pointer_button_down_on())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let v = ((plot.bottom() - pos.y) / plot.height() * 100.0).round().clamp(0.0, 100.0) as u8;
                edited = Some((i, v.max(FanCurve::floor(i))));
            }
            let v = draft[i] as f32;
            let shown = ui.ctx().animate_value_with_time(Id::new(("curve_v", i)), v, 0.12);
            let top = y(shown);
            let col = heat(i as f32 / (CURVE_POINTS - 1) as f32);
            let bar_r = Rect::from_min_max(pos2(cx - bw / 2.0, top), pos2(cx + bw / 2.0, plot.bottom()));
            vgradient(p, bar_r.shrink2(vec2(0.0, 3.0)).translate(vec2(0.0, 3.0)), col.gamma_multiply(0.35), col.gamma_multiply(0.04));
            p.rect_filled(Rect::from_min_size(bar_r.min, vec2(bw, 4.0)), 2.0, col);
            if i >= CURVE_POINTS - 2 {
                // the floor these points can't go below
                let fy = y(FanCurve::floor(i) as f32);
                p.line_segment([pos2(cx - bw / 2.0 - 4.0, fy), pos2(cx + bw / 2.0 + 4.0, fy)], Stroke::new(1.5, FAINT));
            }
            let hover = resp.hovered() || resp.dragged();
            p.circle_filled(pos2(cx, top), if hover { 9.0 } else { 7.0 }, col.gamma_multiply(0.3));
            p.circle_filled(pos2(cx, top), if hover { 6.0 } else { 5.0 }, Color32::WHITE);
            p.text(pos2(cx, top - 16.0), Align2::CENTER_CENTER, format!("{}%", draft[i]), semibold(12.5), TEXT);
            if hover {
                // what that percentage means on this laptop (measured)
                let rpm = (f32::from(draft[i]) * FanCurve::RPM_PER_PCT / 10.0).round() as u32 * 10;
                p.text(pos2(cx, top - 31.0), Align2::CENTER_CENTER, format!("≈ {} RPM", thousands(rpm)), regular(11.0), DIM);
            }
            p.text(pos2(cx, plot.bottom() + 16.0), Align2::CENTER_CENTER, step_label(&st.cpu_curve_temps, i), semibold(12.0), if i == cpu_step { CPU_C } else { DIM });
            p.text(pos2(cx, plot.bottom() + 33.0), Align2::CENTER_CENTER, step_label(&st.gpu_curve_temps, i), regular(11.0), if i == gpu_step { GPU_C } else { FAINT });
        }
        if let Some((i, v)) = edited {
            // keep the curve rising: push neighbours out of the way
            draft[i] = v;
            for d in &mut draft[i + 1..] {
                *d = (*d).max(v);
            }
            for d in &mut draft[..i] {
                *d = (*d).min(v);
            }
        }
        self.draft = (draft != saved).then_some(draft);

        // bottom row: status + buttons
        let by = ed.bottom() - 30.0;
        let custom_on = st.fan_mode == Ok(FanMode::Advanced);
        let (msg, col) = if custom_on && st.cpu_curve == draft {
            ("Custom curve is active".to_string(), COOL)
        } else if self.draft.is_some() || custom_on {
            ("Not applied yet".to_string(), WARM)
        } else {
            (format!("Fan is on {}  ·  Apply switches to Custom", st.fan_mode.map_or("?", fan_name)), DIM)
        };
        status_line(p, pos2(ed.left() + 18.0, by), &msg, col);
        let apply_r = Rect::from_min_size(pos2(ed.right() - 18.0 - 110.0, by - 17.0), vec2(110.0, 34.0));
        let reset_r = apply_r.translate(vec2(-100.0, 0.0)).with_max_x(apply_r.left() - 10.0);
        if button(ui, reset_r, "curve_reset", "Undo", false, self.draft.is_some()) {
            self.draft = None;
        }
        let needs_apply = !(custom_on && st.cpu_curve == draft && st.gpu_curve == draft);
        if button(ui, apply_r, "curve_apply", "Apply", true, can_write && needs_apply) {
            match FanCurve::new(draft) {
                Some(c) => self.send(Cmd::Curve(c)),
                None => self.shared.lock().unwrap().message = Some(("That curve breaks the safety rules".into(), false, Instant::now())),
            }
        }

        } // (curve editor)

        // ---- fan mode
        card_title(p, mode_r, "Fan mode");
        let seg = Rect::from_min_size(mode_r.left_top() + vec2(18.0, 46.0), vec2(side_w - 36.0, 38.0));
        self.fan_choice(ui, seg, "fan2", st, s.smart, can_write);
        let desc = match st.fan_mode {
            _ if s.smart => "Learns your laptop and sets the speed itself",
            Ok(FanMode::Auto) => "MSI's factory curve",
            Ok(FanMode::Silent) => "Quietest; the laptop runs warmer",
            Ok(FanMode::Advanced) => "Your curve on the left",
            _ => "",
        };
        p.text(pos2(seg.left() + 2.0, seg.bottom() + 14.0), Align2::LEFT_CENTER, desc, regular(12.0), FAINT);
        let row = Rect::from_min_size(pos2(mode_r.left() + 18.0, mode_r.bottom() - 58.0), vec2(side_w - 36.0, 44.0));
        if switch_row(ui, p, row, "boost2", "Cooler Boost", "Fan at full speed", st.cooler_boost, can_write) {
            self.send(Cmd::Boost(!st.cooler_boost));
        }

        // ---- right now
        card_title(p, now_r, "Right now");
        let rpm = if st.fan_rpm == 0 { "fan stopped".to_string() } else { format!("{} RPM", thousands(u32::from(st.fan_rpm))) };
        p.text(pos2(now_r.right() - 18.0, now_r.top() + 20.0), Align2::RIGHT_TOP, rpm, semibold(13.0), DIM);
        for (k, (name, temp, fan, step, col)) in
            [("CPU", st.cpu_temp_c, st.cpu_fan_pct, cpu_step, CPU_C), ("GPU", st.gpu_temp_c, st.gpu_fan_pct, gpu_step, GPU_C)].into_iter().enumerate()
        {
            let yy = now_r.top() + 52.0 + k as f32 * 70.0;
            p.circle_filled(pos2(now_r.left() + 22.0, yy + 7.0), 3.5, col);
            p.text(pos2(now_r.left() + 32.0, yy), Align2::LEFT_TOP, format!("{name}  {temp}°C"), semibold(13.5), TEXT);
            p.text(pos2(now_r.right() - 18.0, yy), Align2::RIGHT_TOP, format!("target {fan}%"), semibold(13.5), TEXT);
            bar(p, Rect::from_min_size(pos2(now_r.left() + 18.0, yy + 26.0), vec2(side_w - 36.0, 6.0)), fan as f32 / 100.0, col);
            p.text(pos2(now_r.left() + 18.0, yy + 44.0), Align2::LEFT_CENTER, format!("step {} of {CURVE_POINTS}", step + 1), regular(11.5), FAINT);
        }
        if st.cooler_boost {
            status_line(p, pos2(now_r.left() + 18.0, now_r.bottom() - 20.0), "Cooler Boost overrides the curve", ACCENT);
        } else {
            p.text(pos2(now_r.left() + 18.0, now_r.bottom() - 20.0), Align2::LEFT_CENTER, "The fan takes up to 30 s to change speed", regular(11.5), FAINT);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tools(&mut self, ui: &mut Ui, p: &Painter, area: Rect, st: Option<&Status>, s: &Settings, plans: &[power::Plan], active: Option<power::Guid>, camera: bool, can_write: bool) {
        let w3 = (area.width() - 2.0 * GAP) / 3.0;
        let row_h = 196.0;
        let cell = |i: usize| Rect::from_min_size(pos2(area.left() + i as f32 * (w3 + GAP), area.top()), vec2(w3, row_h));
        let (cam, key, plan_r) = (cell(0), cell(1), cell(2));
        let scen = Rect::from_min_max(pos2(area.left(), area.top() + row_h + GAP), area.max);
        for r in [cam, key, plan_r, scen] {
            card(p, r);
        }
        let sw_at = |r: Rect| Rect::from_min_size(pos2(r.right() - 18.0 - 52.0, r.top() + 14.0), vec2(52.0, 28.0));
        let lines = |r: Rect, a: &str, b: &str| {
            p.text(r.left_top() + vec2(18.0, 54.0), Align2::LEFT_TOP, a, regular(12.5), DIM);
            p.text(r.left_top() + vec2(18.0, 72.0), Align2::LEFT_TOP, b, regular(12.5), DIM);
        };

        // ---- webcam
        card_title(p, cam, "Webcam");
        let cam_on = st.is_some_and(|s| s.webcam_on);
        if switch(ui, sw_at(cam), Id::new("webcam"), cam_on, can_write && st.is_some()) {
            self.send(Cmd::Webcam(!cam_on));
        }
        lines(cam, "Cuts the camera's power in hardware.", "No app can switch it on behind your back.");
        field(p, cam.left_top() + vec2(18.0, 108.0), "POWER", if cam_on { "On" } else { "Off" }, TEXT);
        field(p, cam.left_top() + vec2(130.0, 108.0), "WINDOWS", if camera { "Camera found" } else { "No camera" }, TEXT);
        let (msg, col) = match (cam_on, camera) {
            (true, true) => ("Camera ready", COOL),
            (true, false) => ("Power on, but Windows sees no camera", WARM),
            (false, _) => ("Camera off", FAINT),
        };
        status_line(p, pos2(cam.left() + 18.0, cam.bottom() - 22.0), msg, col);

        // ---- windows key
        card_title(p, key, "Windows key lock");
        if switch(ui, sw_at(key), Id::new("winlock"), s.win_lock, self.winkey_ok) {
            let on = !s.win_lock;
            winkey::set_locked(on);
            self.send(Cmd::WinLock(on));
        }
        lines(key, "Blocks the Windows key so a stray press", "can't throw you out of a game.");
        field(p, key.left_top() + vec2(18.0, 108.0), "STATE", if winkey::is_locked() { "Locked" } else { "Unlocked" }, TEXT);
        field(p, key.left_top() + vec2(130.0, 108.0), "WORKS", "While DragonSlayer runs", TEXT);
        let (msg, col) = if !self.winkey_ok {
            ("Unavailable", FAINT)
        } else if winkey::is_locked() {
            ("Locked  ·  Win+L, Win+D etc. are off too", ACCENT)
        } else {
            ("Unlocked", FAINT)
        };
        status_line(p, pos2(key.left() + 18.0, key.bottom() - 22.0), msg, col);

        // ---- power plan
        card_title(p, plan_r, "Power plan");
        let shown: Vec<&power::Plan> = plans.iter().take(3).collect();
        if shown.is_empty() {
            p.text(plan_r.center(), Align2::CENTER_CENTER, if self.mock { "Not shown in demo mode" } else { "Reading plans..." }, regular(12.5), FAINT);
        } else {
            let labels: Vec<String> = shown.iter().map(|pl| pl.short()).collect();
            let lrefs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let sel = shown.iter().position(|pl| Some(pl.guid) == active);
            let seg = Rect::from_min_size(plan_r.left_top() + vec2(18.0, 54.0), vec2(w3 - 36.0, 38.0));
            if let Some(i) = segmented(ui, seg, Id::new("plan"), &lrefs, sel, true) {
                self.send(Cmd::Plan(shown[i].guid));
            }
            let name = plans.iter().find(|pl| Some(pl.guid) == active).map_or("Unknown".to_string(), |pl| pl.name.clone());
            field(p, plan_r.left_top() + vec2(18.0, 108.0), "ACTIVE PLAN", &name, TEXT);
        }
        p.text(pos2(plan_r.left() + 18.0, plan_r.bottom() - 22.0), Align2::LEFT_CENTER, "Windows' own power plans: how hard the CPU may boost", regular(11.5), FAINT);

        // ---- scenarios
        card_title(p, scen, "Scenario");
        p.text(scen.left_top() + vec2(110.0, 18.0), Align2::LEFT_TOP, "one click sets the power plan and the fan together", regular(12.0), FAINT);
        let tw = (scen.width() - 36.0 - 3.0 * GAP) / 4.0;
        for (i, sc) in Scenario::ALL.into_iter().enumerate() {
            let t = Rect::from_min_size(pos2(scen.left() + 18.0 + i as f32 * (tw + GAP), scen.top() + 50.0), vec2(tw, scen.height() - 68.0));
            let plan = sc.plan(plans);
            let active_now = st.is_some_and(|st| {
                plan.is_some_and(|pl| Some(pl.guid) == active) && st.fan_mode == Ok(sc.fan()) && st.cooler_boost == sc.boost()
            });
            let resp = ui.interact(t, Id::new(("scenario", i)), if can_write { Sense::click() } else { Sense::hover() });
            let hover = can_write && resp.hovered();
            p.rect_filled(t, 12.0, if active_now { ACCENT.gamma_multiply(0.14) } else if hover { Color32::from_white_alpha(8) } else { WELL });
            p.rect_stroke(t, 12.0, Stroke::new(if active_now { 1.5 } else { 1.0 }, if active_now { ACCENT } else { BORDER }), StrokeKind::Inside);
            let icon_c = pos2(t.left() + 26.0, t.top() + 28.0);
            let col = heat(i as f32 / 3.0);
            p.circle_filled(icon_c, 10.0, col.gamma_multiply(0.18));
            p.circle_filled(icon_c, 4.5, col);
            p.text(pos2(t.left() + 44.0, t.top() + 28.0), Align2::LEFT_CENTER, sc.name(), semibold(15.5), TEXT);
            let plan_name = plan.map_or("Balanced".into(), |pl| pl.short());
            p.text(pos2(t.left() + 18.0, t.top() + 62.0), Align2::LEFT_CENTER, format!("{plan_name} power"), regular(12.5), DIM);
            p.text(pos2(t.left() + 18.0, t.top() + 82.0), Align2::LEFT_CENTER, sc.fans_text(), regular(12.5), DIM);
            let blurb = match sc {
                Scenario::Silent => "Quiet for study and films",
                Scenario::Balanced => "Everyday use",
                Scenario::Performance => "Games and heavy work",
                Scenario::Extreme => "Everything maxed, loud",
            };
            p.text(pos2(t.left() + 18.0, t.bottom() - 20.0), Align2::LEFT_CENTER, blurb, regular(11.5), FAINT);
            if active_now {
                p.text(pos2(t.right() - 14.0, t.top() + 28.0), Align2::RIGHT_CENTER, "ACTIVE", semibold(10.5), ACCENT);
            }
            if can_write && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.send(Cmd::Scenario(sc));
            }
        }
    }

    fn automation(&mut self, ui: &mut Ui, p: &Painter, area: Rect, st: Option<&Status>, s: &Settings, auto_boosted: bool) {
        let half = (area.width() - GAP) / 2.0;
        let row_h = 210.0;
        let heat = Rect::from_min_size(area.min, vec2(half, row_h));
        let unplug = Rect::from_min_size(pos2(area.left() + half + GAP, area.top()), vec2(half, row_h));
        let app = Rect::from_min_max(pos2(area.left(), area.top() + row_h + GAP), area.max);
        for r in [heat, unplug, app] {
            card(p, r);
        }

        // heat protection
        card_title(p, heat, "Heat protection");
        let sw = Rect::from_min_size(pos2(heat.right() - 18.0 - 52.0, heat.top() + 14.0), vec2(52.0, 28.0));
        if switch(ui, sw, Id::new("heat"), s.heat_protect, true) {
            self.send(Cmd::HeatProtect(!s.heat_protect));
        }
        p.text(heat.left_top() + vec2(18.0, 50.0), Align2::LEFT_TOP, "Turns Cooler Boost on when the CPU or GPU reaches the limit,", regular(12.5), DIM);
        p.text(heat.left_top() + vec2(18.0, 68.0), Align2::LEFT_TOP, "and off again once it has cooled 12°C below it for 10 seconds.", regular(12.5), DIM);
        p.text(heat.left_top() + vec2(18.0, 100.0), Align2::LEFT_TOP, "LIMIT", semibold(10.5), FAINT);
        let seg = Rect::from_min_size(heat.left_top() + vec2(18.0, 118.0), vec2(300.0, 38.0));
        let labels: Vec<String> = HEAT_LIMITS.iter().map(|l| format!("{l}°C")).collect();
        let lrefs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let sel = HEAT_LIMITS.iter().position(|&l| l == s.heat_limit);
        if let Some(i) = segmented(ui, seg, Id::new("heat_limit"), &lrefs, sel, true) {
            self.send(Cmd::HeatLimit(HEAT_LIMITS[i]));
        }
        let hot = st.map(|st| st.cpu_temp_c.max(st.gpu_temp_c));
        let (line, col) = if !s.heat_protect {
            ("Off".to_string(), FAINT)
        } else if auto_boosted {
            ("Cooling down: Cooler Boost is on".to_string(), WARM)
        } else {
            (hot.map_or("Watching".into(), |h| format!("Watching  ·  hottest right now {h}°C")), COOL)
        };
        p.circle_filled(pos2(heat.left() + 22.0, heat.bottom() - 24.0), 3.5, col);
        p.text(pos2(heat.left() + 32.0, heat.bottom() - 24.0), Align2::LEFT_CENTER, line, semibold(12.5), col);

        // when unplugged
        card_title(p, unplug, "When unplugged");
        p.text(pos2(unplug.right() - 18.0, unplug.top() + 27.0), Align2::RIGHT_CENTER, "plugging in puts your settings back", regular(11.5), FAINT);
        let row = |i: f32| Rect::from_min_size(pos2(unplug.left() + 18.0, unplug.top() + 50.0 + i * 50.0), vec2(unplug.width() - 36.0, 44.0));
        let quiet_desc = if s.smart { "Not needed: Smart mode leans quiet on battery" } else { "Fan switches to Silent on battery" };
        if switch_row(ui, p, row(0.0), "bat_quiet", "Quiet fan", quiet_desc, s.battery_quiet, !s.smart) {
            self.send(Cmd::BatteryQuiet(!s.battery_quiet));
        }
        if switch_row(ui, p, row(1.0), "bat_kbd", "Keyboard light off", "Saves power on battery", s.battery_kbd_off, true) {
            self.send(Cmd::BatteryKbdOff(!s.battery_kbd_off));
        }
        // screen refresh rate: slow on battery, fast when it matters
        let (hz, rates) = {
            let sh = self.shared.lock().unwrap();
            (sh.hz, sh.rates.clone())
        };
        let r = row(2.0);
        let (slow, fast) = (rates.first().copied().unwrap_or(0), rates.last().copied().unwrap_or(0));
        let can_switch = rates.len() >= 2;
        let desc = if !can_switch {
            "This screen has one refresh rate only".to_string()
        } else {
            match s.refresh {
                Refresh::Off => format!("Left alone  ·  now {} Hz  ·  screen offers {}", hz.unwrap_or(0), rates.iter().map(u32::to_string).collect::<Vec<_>>().join(" / ")),
                Refresh::Battery => format!("{slow} Hz on battery, {fast} Hz plugged in  ·  now {} Hz", hz.unwrap_or(0)),
                Refresh::Adaptive => format!("{slow} Hz on battery, fast for games  ·  now {} Hz", hz.unwrap_or(0)),
            }
        };
        p.text(pos2(r.left(), r.center().y - 9.0), Align2::LEFT_CENTER, "Screen refresh", semibold(14.0), TEXT);
        p.text(pos2(r.left(), r.center().y + 10.0), Align2::LEFT_CENTER, desc, regular(12.0), FAINT);
        let seg = Rect::from_min_size(pos2(r.right() - 222.0, r.center().y - 17.0), vec2(222.0, 34.0));
        let sel = Refresh::ALL.iter().position(|&x| x == s.refresh);
        if let Some(i) = segmented(ui, seg, Id::new("refresh"), &["Off", "Battery", "Adaptive"], sel, can_switch) {
            self.send(Cmd::Refresh(Refresh::ALL[i]));
        }

        // app
        card_title(p, app, "App");
        let colw = (app.width() - 36.0 - 2.0 * 40.0) / 3.0;
        let col_r = |i: f32| Rect::from_min_size(pos2(app.left() + 18.0 + i * (colw + 40.0), app.top() + 56.0), vec2(colw, 48.0));
        if switch_row(ui, p, col_r(0.0), "autostart2", "Start with Windows", "Starts hidden in the tray at login", self.autostart, !self.mock) {
            self.toggle_autostart();
        }
        let tray_ok = self.tray.is_some();
        if switch_row(ui, p, col_r(1.0), "tray", "Close to tray", if tray_ok { "X hides the window; quit from the tray" } else { "Tray icon unavailable" }, s.close_to_tray, tray_ok) {
            self.send(Cmd::CloseToTray(!s.close_to_tray));
        }
        let c2 = col_r(2.0);
        p.text(pos2(c2.left(), c2.center().y - 9.0), Align2::LEFT_CENTER, "Saved settings", semibold(14.0), TEXT);
        let path = Settings::path().map_or("-".into(), |p| p.display().to_string());
        let path = path.replace(&std::env::var("APPDATA").unwrap_or_default(), "%APPDATA%");
        p.text(pos2(c2.left(), c2.center().y + 10.0), Align2::LEFT_CENTER, path, regular(12.0), FAINT);
        p.text(pos2(app.left() + 18.0, app.bottom() - 22.0), Align2::LEFT_CENTER, "Your fan, battery and keyboard choices are re-applied every time DragonSlayer starts.", regular(12.0), FAINT);
    }

    #[allow(clippy::too_many_arguments)]
    fn strip(&mut self, ui: &mut Ui, p: &Painter, info: Rect, st: Option<&Status>, sys: &SysSnap, can_write: bool, backend: &str) {
        card(p, info);
        let power = match sys.power {
            Some((ac, pct)) => format!("{}  ·  {pct}%", if ac { "Plugged in" } else { "On battery" }),
            None => "Unknown".to_string(),
        };
        let items = [
            ("POWER", power),
            ("EC FIRMWARE", st.map_or("—".into(), |s| s.firmware.clone())),
            ("HARDWARE ACCESS", format!("{backend}  ·  {}", if can_write { "read & write" } else { "read-only" })),
        ];
        let iw = info.width() / (items.len() + 1) as f32;
        for (i, (k, v)) in items.iter().enumerate() {
            let xl = info.left() + i as f32 * iw;
            if i > 0 {
                p.line_segment([pos2(xl, info.top() + 14.0), pos2(xl, info.bottom() - 14.0)], Stroke::new(1.0, BORDER));
            }
            field(p, pos2(xl + 20.0, info.top() + 13.0), k, v, TEXT);
        }
        let xl = info.left() + items.len() as f32 * iw;
        p.line_segment([pos2(xl, info.top() + 14.0), pos2(xl, info.bottom() - 14.0)], Stroke::new(1.0, BORDER));
        field(p, pos2(xl + 20.0, info.top() + 13.0), "START WITH WINDOWS", if self.autostart { "On" } else { "Off" }, TEXT);
        let sw = Rect::from_min_size(pos2(info.right() - 20.0 - 46.0, info.center().y - 12.0), vec2(46.0, 24.0));
        if switch(ui, sw, Id::new("autostart"), self.autostart, !self.mock) {
            self.toggle_autostart();
        }
    }
}

fn footer(ui: &Ui, p: &Painter, foot: Rect, left: Option<(String, Color32)>, message: Option<(String, bool, Instant)>) {
    if let Some((text, col)) = left {
        p.circle_filled(pos2(foot.left() + 4.0, foot.center().y), 3.5, col);
        p.text(pos2(foot.left() + 14.0, foot.center().y), Align2::LEFT_CENTER, text, regular(12.0), DIM);
    }
    if let Some((msg, ok, at)) = message {
        let age = at.elapsed().as_secs_f32();
        if age < 4.0 {
            let a = if age > 3.0 { 4.0 - age } else { 1.0 };
            let col = if ok { COOL } else { HOT };
            let g = p.layout_no_wrap(msg, semibold(12.0), TEXT.gamma_multiply(a));
            let w = g.size().x + 30.0;
            let pill = Rect::from_min_size(pos2(foot.right() - w, foot.center().y - 12.0), vec2(w, 24.0));
            p.rect_filled(pill, 12.0, col.gamma_multiply(0.14 * a));
            p.circle_filled(pos2(pill.left() + 13.0, pill.center().y), 3.5, col.gamma_multiply(a));
            p.galley(pos2(pill.left() + 22.0, pill.center().y - g.size().y / 2.0), g, TEXT);
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}

enum TrayDo {
    Show,
    Quit,
    Boost(bool),
    Silent(bool),
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // tray clicks and menu picks
        let mut todo = Vec::new();
        if self.show_req.swap(false, Ordering::Relaxed) {
            todo.push(TrayDo::Show);
        }
        if let Some((_, menu, rx)) = &self.tray {
            while let Ok(m) = rx.try_recv() {
                todo.push(match m {
                    TrayMsg::Show => TrayDo::Show,
                    TrayMsg::Menu(id) if id == menu.open => TrayDo::Show,
                    TrayMsg::Menu(id) if id == menu.quit => TrayDo::Quit,
                    TrayMsg::Menu(id) if id == *menu.boost.id() => TrayDo::Boost(menu.boost.is_checked()),
                    TrayMsg::Menu(id) if id == *menu.silent.id() => TrayDo::Silent(menu.silent.is_checked()),
                    TrayMsg::Menu(_) => continue,
                });
            }
        }
        for d in todo {
            match d {
                TrayDo::Show => {
                    self.hidden = false;
                    Self::show_window(ctx);
                }
                TrayDo::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                TrayDo::Boost(on) => self.send(Cmd::Boost(on)),
                TrayDo::Silent(on) => self.send(Cmd::Fan(if on { FanMode::Silent } else { FanMode::Auto })),
            }
        }

        // X button / Alt+F4: hide to the tray instead of quitting
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.close_to_tray() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.hidden = true;
        }

        // keep the tray tooltip and check marks current
        let (st, smart_on) = {
            let s = self.shared.lock().unwrap();
            (s.status.clone(), s.settings.smart)
        };
        if let (Some((tray, menu, _)), Some(st)) = (&self.tray, st) {
            let fans = if smart_on { "Smart" } else { st.fan_mode.map_or("Unknown", fan_name) };
            let boost = if st.cooler_boost { "  ·  Cooler Boost" } else { "" };
            // RPM rounded to 100 so the tooltip doesn't churn on tach jitter
            let rpm = thousands((u32::from(st.fan_rpm) + 50) / 100 * 100);
            let tip = format!("DragonSlayer\nCPU {}°C  ·  GPU {}°C\nFan {fans}  ·  {rpm} RPM{boost}", st.cpu_temp_c, st.gpu_temp_c);
            if tip != self.last_tip {
                let _ = tray.set_tooltip(Some(&tip));
                menu.boost.set_checked(st.cooler_boost);
                menu.silent.set_checked(matches!(st.fan_mode, Ok(FanMode::Silent)));
                self.last_tip = tip;
            }
        }
    }

    fn ui(&mut self, root: &mut Ui, frame: &mut eframe::Frame) {
        self.frames += 1;
        // DS_FRAME_LOG=<file>: every 5 s, frames drawn and average CPU time
        // per frame as eframe measures it (our code + paint, without vsync)
        if let Some(path) = std::env::var_os("DS_FRAME_LOG") {
            use std::io::Write;
            let now = root.ctx().input(|i| i.time);
            let (n, sum, since) = &mut self.frame_log;
            *n += 1;
            *sum += frame.info().cpu_usage.unwrap_or(0.0);
            if now - *since >= 5.0 {
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(f, "{:.1} fps, {:.2} ms per frame", f64::from(*n) / (now - *since), *sum * 1000.0 / *n as f32);
                }
                self.frame_log = (0, 0.0, now);
            }
        }
        if self.frames == 2 && self.startup {
            let cmd = if self.close_to_tray() { ViewportCommand::Visible(false) } else { ViewportCommand::Minimized(true) };
            root.ctx().send_viewport_cmd(cmd);
            self.hidden = true;
        }
        // animation frames only while the Dashboard is on screen
        let minimized = root.ctx().input(|i| i.viewport().minimized.unwrap_or(false));
        if self.tab == Tab::Dashboard && !self.hidden && !minimized {
            // a steady 60 fps. Not request_repaint(): on a 144 Hz screen that
            // redraws 144 times a second and the driver's vsync wait burns a
            // whole CPU core. (egui subtracts predicted_dt from the delay.)
            let predicted = root.ctx().input(|i| i.predicted_dt);
            root.ctx().request_repaint_after(Duration::from_secs_f32(1.0 / 60.0 + predicted));
        }
        // demo: `--smart` shows Smart mode switched on (for screenshots)
        if self.frames == 1 && self.mock && std::env::args().any(|a| a == "--smart") {
            self.send(Cmd::Smart(true));
        }
        if self.frames == 3 {
            win::round_corners();
            if !self.startup && std::env::args().any(|a| a == "--maximised") {
                win::set_maximised(true);
            }
        }
        let (status, history, error, message, can_write, dc, backend, sys, settings, auto_boosted) = {
            let s = self.shared.lock().unwrap();
            (
                s.status.clone(),
                s.history.clone(),
                s.error.clone(),
                s.message.clone(),
                s.can_write,
                s.dragon_center_running,
                s.backend,
                s.sys.clone(),
                s.settings.clone(),
                s.auto_boosted,
            )
        };
        let (plans, active_plan, camera) = {
            let s = self.shared.lock().unwrap();
            (s.plans.clone(), s.active_plan, s.camera)
        };
        fit_zoom(root);

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(BG)).show(root, |ui| {
            let full = ui.max_rect();
            let p = ui.painter().clone();
            let now = ui.ctx().input(|i| i.time);
            // ambient glow behind the title bar: stronger as the laptop gets
            // hotter, breathing slowly (the Dashboard redraws continuously)
            let hottest = status.as_ref().map_or(50.0, |s| f32::from(s.cpu_temp_c.max(s.gpu_temp_c)));
            let heat_now = ease(ui.ctx(), Id::new("ambient"), temp_frac(hottest), 0.8);
            let breathe = 0.5 + 0.5 * (now as f32 * (0.6 + 1.2 * heat_now)).sin();
            let glow = 0.06 + 0.09 * heat_now + 0.025 * breathe * (0.4 + heat_now);
            vgradient(&p, Rect::from_min_size(full.min, vec2(full.width(), 230.0 + 60.0 * heat_now)), ACCENT.gamma_multiply(glow), Color32::TRANSPARENT);

            let mut tab = self.tab;
            let tb = title_bar(ui, full, self.mock, &mut tab);
            if tab != self.tab {
                self.tab_since = now;
            }
            self.tab = tab;
            // a new tab's content fades in and settles upward
            let t = (((now - self.tab_since) / 0.24) as f32).clamp(0.0, 1.0);
            let enter = 1.0 - (1.0 - t).powi(3);
            if t < 1.0 {
                ui.ctx().request_repaint();
            }

            let inner_w = full.width() - 2.0 * PAD;
            let x0 = full.left() + PAD;
            // maximised on a taller-than-designed screen: centre the content
            let spare = ((full.height() - WIN_H) / 2.0).max(0.0);
            let area = Rect::from_min_size(pos2(x0, tb.bottom() + 12.0 + spare), vec2(inner_w, 410.0));
            let info = Rect::from_min_size(pos2(x0, area.bottom() + GAP), vec2(inner_w, 64.0));
            let foot = Rect::from_min_size(pos2(x0, info.bottom() + 6.0), vec2(inner_w, 24.0));

            let strip_p = p.clone();
            ui.set_opacity(enter);
            let p = ui.painter().clone();
            let area = area.translate(vec2(0.0, (1.0 - enter) * 14.0));
            match self.tab {
                Tab::Dashboard => match &status {
                    Some(st) => self.dashboard(ui, &p, area, st, &history, can_write),
                    None => connecting(&p, area, error.as_deref()),
                },
                Tab::Fans => match &status {
                    Some(st) => self.fans(ui, &p, area, st, &settings, can_write),
                    None => connecting(&p, area, error.as_deref()),
                },
                Tab::Tools => self.tools(ui, &p, area, status.as_ref(), &settings, &plans, active_plan, camera, can_write),
                Tab::System => self.system(&p, area, status.as_ref(), &sys),
                Tab::Automation => self.automation(ui, &p, area, status.as_ref(), &settings, auto_boosted),
            }
            ui.set_opacity(1.0);
            let p = strip_p;
            self.strip(ui, &p, info, status.as_ref(), &sys, can_write, backend);

            let left = if status.is_some() && error.is_some() {
                Some((format!("Read error: {}", error.unwrap_or_default()), HOT))
            } else if status.is_some() && !can_write {
                Some(("Read-only mode".to_string(), WARM))
            } else if dc {
                Some(("Dragon Center is running and may undo changes".to_string(), WARM))
            } else {
                None
            };
            footer(ui, &p, foot, left, message);
        });
    }
}
