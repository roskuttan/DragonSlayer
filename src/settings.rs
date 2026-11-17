//! The user's chosen settings, remembered between runs and re-applied when
//! the app starts (Dragon Center does the same at login).
//!
//! Stored as plain `key=value` lines in `%APPDATA%\DragonSlayer\settings.txt`.
//! Hardware settings are only stored once the user actually changed them.

use std::path::{Path, PathBuf};

use crate::controller::Controller;
use crate::ec::backend::EcBackend;
use crate::ec::map::{ChargeLimit, FanCurve, FanMode, KbdBacklight};

/// Heat-protection thresholds the UI offers.
pub const HEAT_LIMITS: [u8; 3] = [85, 90, 95];

/// When the app changes the screen refresh rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Refresh {
    /// never: leave the screen alone
    #[default]
    Off,
    /// slow rate on battery, fast when plugged in
    Battery,
    /// like Battery, but fast on battery too while a heavy program is in front
    Adaptive,
}

impl Refresh {
    pub const ALL: [Refresh; 3] = [Refresh::Off, Refresh::Battery, Refresh::Adaptive];
    pub fn name(self) -> &'static str {
        match self {
            Refresh::Off => "off",
            Refresh::Battery => "battery",
            Refresh::Adaptive => "adaptive",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    // hardware (None = never changed by the user, leave alone)
    pub fan: Option<FanMode>,
    /// The user's custom curve (used when `fan` is Advanced, shown as "Custom").
    pub curve: Option<FanCurve>,
    pub boost: Option<bool>,
    pub charge: Option<u8>,
    pub kbd: Option<u8>,
    // software-only
    pub win_lock: bool,
    /// Smart fan mode: the app sets the speed itself (src/smart.rs). `fan`
    /// and `curve` stay as the user left them, for when it is switched off.
    pub smart: bool,
    /// Smart mode preference in degrees: + quieter, - cooler (-6..=6).
    pub smart_bias: i8,
    /// Screen refresh rate switching (src/display.rs).
    pub refresh: Refresh,
    // automation / app behaviour
    pub heat_protect: bool,
    pub heat_limit: u8,
    pub battery_quiet: bool,
    pub battery_kbd_off: bool,
    pub close_to_tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fan: None,
            curve: None,
            boost: None,
            charge: None,
            kbd: None,
            win_lock: false,
            smart: false,
            smart_bias: 0,
            refresh: Refresh::Off,
            heat_protect: false,
            heat_limit: 90,
            battery_quiet: false,
            battery_kbd_off: false,
            close_to_tray: true,
        }
    }
}

fn on_off(v: &str) -> Option<bool> {
    match v {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "fan" => {
                    s.fan = match v {
                        "auto" => Some(FanMode::Auto),
                        "silent" => Some(FanMode::Silent),
                        "custom" => Some(FanMode::Advanced),
                        _ => None,
                    }
                }
                "curve" => s.curve = FanCurve::parse(v),
                "win_lock" => s.win_lock = on_off(v).unwrap_or(s.win_lock),
                "smart" => s.smart = on_off(v).unwrap_or(s.smart),
                "smart_bias" => s.smart_bias = v.parse::<i8>().map_or(s.smart_bias, |b| b.clamp(-6, 6)),
                "refresh" => s.refresh = Refresh::parse(v).unwrap_or(s.refresh),
                "boost" => s.boost = on_off(v),
                "charge" => s.charge = v.parse().ok().filter(|p| ChargeLimit::new(*p).is_some()),
                "kbd" => s.kbd = v.parse().ok().filter(|l| KbdBacklight::new(*l).is_some()),
                "heat_protect" => s.heat_protect = on_off(v).unwrap_or(s.heat_protect),
                "heat_limit" => {
                    if let Some(l) = v.parse().ok().filter(|l| HEAT_LIMITS.contains(l)) {
                        s.heat_limit = l;
                    }
                }
                "battery_quiet" => s.battery_quiet = on_off(v).unwrap_or(s.battery_quiet),
                "battery_kbd_off" => s.battery_kbd_off = on_off(v).unwrap_or(s.battery_kbd_off),
                "close_to_tray" => s.close_to_tray = on_off(v).unwrap_or(s.close_to_tray),
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        let b = |x: bool| if x { "on" } else { "off" };
        let mut out = String::new();
        if let Some(f) = self.fan {
            let name = match f {
                FanMode::Silent => "silent",
                FanMode::Advanced => "custom",
                _ => "auto",
            };
            out.push_str(&format!("fan={name}\n"));
        }
        if let Some(c) = self.curve {
            out.push_str(&format!("curve={}\n", c.to_text()));
        }
        out.push_str(&format!("win_lock={}
", b(self.win_lock)));
        out.push_str(&format!("smart={}
", b(self.smart)));
        out.push_str(&format!("smart_bias={}
", self.smart_bias));
        out.push_str(&format!("refresh={}
", self.refresh.name()));
        if let Some(v) = self.boost {
            out.push_str(&format!("boost={}\n", b(v)));
        }
        if let Some(c) = self.charge {
            out.push_str(&format!("charge={c}\n"));
        }
        if let Some(k) = self.kbd {
            out.push_str(&format!("kbd={k}\n"));
        }
        out.push_str(&format!("heat_protect={}\n", b(self.heat_protect)));
        out.push_str(&format!("heat_limit={}\n", self.heat_limit));
        out.push_str(&format!("battery_quiet={}\n", b(self.battery_quiet)));
        out.push_str(&format!("battery_kbd_off={}\n", b(self.battery_kbd_off)));
        out.push_str(&format!("close_to_tray={}\n", b(self.close_to_tray)));
        out
    }

    /// Where Smart mode keeps what it has learned (next to the settings).
    pub fn learned_path() -> Option<PathBuf> {
        Self::path().map(|p| p.with_file_name("learned.txt"))
    }

    pub fn path() -> Option<PathBuf> {
        Some(Self::path_in(Path::new(&std::env::var_os("APPDATA")?)))
    }

    /// `<base>\DragonSlayer\settings.txt`. The app was called MSI Control
    /// before: its settings move over once. If the old folder is busy (a
    /// scanner holding the file), copy the file instead; if even that fails,
    /// keep using the old file rather than starting from defaults.
    fn path_in(base: &Path) -> PathBuf {
        let dir = base.join("DragonSlayer");
        let file = dir.join("settings.txt");
        let old = base.join("MSI-Control");
        let old_file = old.join("settings.txt");
        if !file.exists() && old_file.exists() {
            if !dir.exists() && std::fs::rename(&old, &dir).is_ok() {
                return file;
            }
            let copied = std::fs::create_dir_all(&dir).and_then(|()| std::fs::copy(&old_file, &file)).is_ok();
            if !copied {
                return old_file;
            }
        }
        file
    }

    pub fn load() -> Self {
        Self::load_checked().0
    }

    /// The saved settings, plus the error if the file exists but couldn't
    /// be read (then the defaults are returned, and the caller should say so
    /// instead of silently behaving as if nothing had ever been set).
    pub fn load_checked() -> (Self, Option<String>) {
        let Some(p) = Self::path() else { return (Self::default(), None) };
        match std::fs::read_to_string(&p) {
            Ok(t) => (Self::parse(&t), None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("{}: {e}", p.display()))),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = Self::path() else { return Ok(()) };
        Self::write_file(&p, &self.to_text())
    }

    /// Writes a fresh file next to `p` and puts it in `p`'s place. Not a
    /// plain overwrite: on this PC the Roaming folder is flagged "encrypted",
    /// and the copy of the app that Task Scheduler starts could not write
    /// into a settings file that carried that flag, although it can create
    /// and replace files (seen 2026-11-17: choices silently not saved).
    pub fn write_file(p: &Path, text: &str) -> std::io::Result<()> {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("new");
        std::fs::write(&tmp, text)?;
        if std::fs::rename(&tmp, p).is_ok() {
            return Ok(());
        }
        // replace refused: remove the old file, then move the new one in
        let _ = std::fs::remove_file(p);
        std::fs::rename(&tmp, p).or_else(|_| {
            let _ = std::fs::remove_file(&tmp);
            std::fs::write(p, text)
        })
    }

    /// True if no hardware setting has been chosen yet.
    pub fn no_hardware(&self) -> bool {
        !self.smart && self.fan.is_none() && self.boost.is_none() && self.charge.is_none() && self.kbd.is_none()
    }

    /// Re-applies every stored hardware setting. Returns (applied, errors).
    pub fn apply<B: EcBackend>(&self, ctl: &mut Controller<B>) -> (usize, Vec<String>) {
        let mut ok = 0;
        let mut errs = Vec::new();
        let mut run = |what: &str, r: crate::ec::backend::EcResult<()>| match r {
            Ok(()) => ok += 1,
            Err(e) => errs.push(format!("{what}: {e}")),
        };
        if self.smart {
            // Smart mode takes over from here; start from a middling speed
            run("Smart fan mode", ctl.set_fan_profile(FanMode::Advanced, Some(crate::smart::curve_for(50))));
        } else if let Some(f) = self.fan {
            run("fan mode", ctl.set_fan_profile(f, self.curve));
        }
        if let Some(b) = self.boost {
            run("Cooler Boost", ctl.set_cooler_boost(b));
        }
        if let Some(c) = self.charge.and_then(ChargeLimit::new) {
            run("battery limit", ctl.set_charge_limit(c));
        }
        if let Some(k) = self.kbd.and_then(KbdBacklight::new) {
            run("keyboard light", ctl.set_kbd_backlight(k));
        }
        (ok, errs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::backend::MockEc;
    use crate::ec::map;

    #[test]
    fn write_file_replaces_and_survives_a_read_only_old_file() {
        let dir = std::env::temp_dir().join(format!("ds-write-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("sub").join("settings.txt");
        Settings::write_file(&p, "a=1
").unwrap();
        Settings::write_file(&p, "a=2
").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a=2
");
        assert!(!p.with_extension("new").exists(), "no temp file left behind");
        // an old file that refuses to be written into still gets replaced
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&p, perm).unwrap();
        assert!(std::fs::write(&p, "x").is_err(), "plain overwrite is refused");
        let _ = Settings::write_file(&p, "a=3
");
        let now = std::fs::read_to_string(&p).unwrap();
        assert!(now == "a=3
" || now == "a=2
", "never a damaged file: {now:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_move_over_from_the_old_name() {
        let base = std::env::temp_dir().join(format!("ds-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let old = base.join("MSI-Control");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.txt"), "kbd=3\n").unwrap();

        let p = Settings::path_in(&base);
        assert_eq!(p, base.join("DragonSlayer").join("settings.txt"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "kbd=3\n", "old settings carried over");
        assert_eq!(Settings::path_in(&base), p, "second call: nothing to move");

        // old folder busy and new folder already half-made: copy instead
        std::fs::remove_dir_all(&base).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.txt"), "kbd=1\n").unwrap();
        std::fs::create_dir_all(base.join("DragonSlayer")).unwrap();
        let p = Settings::path_in(&base);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "kbd=1\n");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn round_trip() {
        let s = Settings {
            fan: Some(FanMode::Advanced),
            curve: Some(FanCurve::QUIET),
            boost: Some(false),
            charge: Some(80),
            kbd: Some(2),
            win_lock: true,
            smart: true,
            smart_bias: -3,
            refresh: Refresh::Adaptive,
            heat_protect: true,
            heat_limit: 85,
            battery_quiet: true,
            battery_kbd_off: false,
            close_to_tray: false,
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn bad_values_are_ignored() {
        let s = Settings::parse("fan=turbo\nboost=maybe\ncharge=5\nkbd=9\njunk\n=x\ncharge=60\nheat_limit=70\ncurve=10,20,30,40,50,60,70");
        assert_eq!(s, Settings { charge: Some(60), ..Default::default() });
    }

    #[test]
    fn empty_settings_write_nothing() {
        let mut c = Controller::new(MockEc::gf63_idle()).unwrap();
        assert_eq!(Settings::default().apply(&mut c), (0, vec![]));
        assert!(c.into_backend().write_log.is_empty());
    }

    #[test]
    fn apply_writes_stored_values() {
        let mut c = Controller::new(MockEc::gf63_idle()).unwrap();
        let s = Settings { fan: Some(FanMode::Silent), charge: Some(80), kbd: Some(1), ..Default::default() };
        assert_eq!(s.apply(&mut c), (3, vec![]));
        let ec = c.into_backend();
        assert_eq!(ec.regs[map::FAN_MODE as usize], 0x1D);
        assert_eq!(ec.regs[map::CHARGE_LIMIT as usize], 0xD0);
        assert_eq!(ec.regs[map::KBD_BACKLIGHT as usize], 0x81);
    }

    #[test]
    fn custom_fan_restores_the_saved_curve() {
        let mut c = Controller::new(MockEc::gf63_idle()).unwrap();
        let s = Settings { fan: Some(FanMode::Advanced), curve: Some(FanCurve::COOL), ..Default::default() };
        assert_eq!(s.apply(&mut c), (1, vec![]));
        let st = c.status().unwrap();
        assert_eq!(st.cpu_curve, FanCurve::COOL.speeds());
        assert_eq!(st.fan_mode, Ok(FanMode::Advanced));
    }
}
