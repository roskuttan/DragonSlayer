//! Things the app does on its own, once per second:
//!
//! * **Heat protection** — Cooler Boost on when CPU or GPU reaches the
//!   limit; off again after it has stayed 12 °C below the limit for 10 s.
//!   Only turns off a boost it turned on itself, and backs off if the user
//!   switches the boost off by hand.
//! * **On battery** — when unplugged: fans Silent and/or keyboard light off.
//!   When plugged back in: the user's own settings are restored.
//!
//! Pure logic (no hardware), so it is unit-tested.

use crate::controller::Status;
use crate::ec::map::FanMode;
use crate::settings::Settings;

const COOL_MARGIN: u8 = 12;
const COOL_SECONDS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Boost(bool),
    Fan(FanMode),
    Kbd(u8),
    /// Re-apply the user's saved hardware settings.
    RestoreUser,
}

#[derive(Debug, Default)]
pub struct Automation {
    auto_boosted: bool,
    cool_ticks: u32,
    last_ac: Option<bool>,
}

impl Automation {
    pub fn auto_boosted(&self) -> bool {
        self.auto_boosted
    }

    /// One step. `ac` = plugged in (None if unknown). Returns actions with a
    /// short human-readable reason each.
    pub fn tick(&mut self, s: &Settings, st: &Status, ac: Option<bool>) -> Vec<(Action, String)> {
        let mut out = Vec::new();
        let hot = st.cpu_temp_c.max(st.gpu_temp_c);

        // --- heat protection
        if self.auto_boosted && !st.cooler_boost {
            // user (or something else) turned it off: respect that
            self.auto_boosted = false;
            self.cool_ticks = 0;
        }
        if s.heat_protect {
            if !st.cooler_boost && hot >= s.heat_limit {
                self.auto_boosted = true;
                self.cool_ticks = 0;
                out.push((Action::Boost(true), format!("Heat protection: Cooler Boost on ({hot}°C)")));
            } else if self.auto_boosted {
                if hot <= s.heat_limit.saturating_sub(COOL_MARGIN) {
                    self.cool_ticks += 1;
                    if self.cool_ticks >= COOL_SECONDS {
                        self.auto_boosted = false;
                        self.cool_ticks = 0;
                        out.push((Action::Boost(false), format!("Heat protection: cooled to {hot}°C, Cooler Boost off")));
                    }
                } else {
                    self.cool_ticks = 0;
                }
            }
        } else if self.auto_boosted {
            self.auto_boosted = false;
            self.cool_ticks = 0;
            out.push((Action::Boost(false), "Heat protection off: Cooler Boost off".into()));
        }

        // --- on battery
        if let Some(now) = ac {
            let changed = self.last_ac != Some(now);
            self.last_ac = Some(now);
            if changed && (s.battery_quiet || s.battery_kbd_off) {
                if now {
                    // (start-up on AC is not a change: see `prime`)
                    out.push((Action::RestoreUser, "Plugged in: your settings restored".into()));
                } else {
                    if s.battery_quiet {
                        out.push((Action::Fan(FanMode::Silent), "On battery: fans set to Silent".into()));
                    }
                    if s.battery_kbd_off {
                        out.push((Action::Kbd(0), "On battery: keyboard light off".into()));
                    }
                }
            }
        }
        out
    }

    /// Call once at start-up with the current power state, so the first
    /// tick does not treat "plugged in" as a change.
    pub fn prime(&mut self, ac: Option<bool>) {
        if ac == Some(true) {
            self.last_ac = Some(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::map::{ChargeLimit, FanCurve, KbdBacklight};

    fn status(cpu: u8, gpu: u8, boost: bool) -> Status {
        Status {
            firmware: "16R5EMS1.102".into(),
            firmware_verified: true,
            cpu_temp_c: cpu,
            cpu_fan_pct: 50,
            gpu_temp_c: gpu,
            gpu_fan_pct: 50,
            fan_rpm: 3000,
            cooler_boost: boost,
            shift_mode: Err(0x80),
            fan_mode: Ok(FanMode::Auto),
            charge_limit: Ok(ChargeLimit::new(100).unwrap()),
            kbd_backlight: Ok(KbdBacklight::new(0).unwrap()),
            webcam_on: false,
            fn_win_swapped: false,
            cpu_curve_temps: [55, 64, 73, 76, 82, 88],
            cpu_curve: FanCurve::FACTORY.speeds(),
            gpu_curve_temps: [55, 61, 67, 73, 79, 85],
            gpu_curve: FanCurve::FACTORY.speeds(),
        }
    }

    fn heat_on() -> Settings {
        Settings { heat_protect: true, heat_limit: 90, ..Default::default() }
    }

    fn actions(v: Vec<(Action, String)>) -> Vec<Action> {
        v.into_iter().map(|(a, _)| a).collect()
    }

    #[test]
    fn boosts_when_hot_and_releases_after_cooling() {
        let mut a = Automation::default();
        let s = heat_on();
        assert!(actions(a.tick(&s, &status(85, 60, false), None)).is_empty());
        assert_eq!(actions(a.tick(&s, &status(91, 60, false), None)), vec![Action::Boost(true)]);
        // still warm (above 78): stays on
        for _ in 0..20 {
            assert!(actions(a.tick(&s, &status(82, 60, true), None)).is_empty());
        }
        // cool for 9 s: nothing yet; 10th second: off
        for _ in 0..9 {
            assert!(actions(a.tick(&s, &status(75, 60, true), None)).is_empty());
        }
        assert_eq!(actions(a.tick(&s, &status(75, 60, true), None)), vec![Action::Boost(false)]);
    }

    #[test]
    fn gpu_heat_counts_too() {
        let mut a = Automation::default();
        assert_eq!(actions(a.tick(&heat_on(), &status(60, 92, false), None)), vec![Action::Boost(true)]);
    }

    #[test]
    fn never_turns_off_a_boost_the_user_turned_on() {
        let mut a = Automation::default();
        for _ in 0..30 {
            assert!(actions(a.tick(&heat_on(), &status(60, 55, true), None)).is_empty());
        }
    }

    #[test]
    fn backs_off_if_user_turns_boost_off() {
        let mut a = Automation::default();
        let s = heat_on();
        a.tick(&s, &status(91, 60, false), None);
        assert!(a.auto_boosted());
        // user switched it off while still at 85 (below limit): leave it off
        assert!(actions(a.tick(&s, &status(85, 60, false), None)).is_empty());
        assert!(!a.auto_boosted());
    }

    #[test]
    fn disabled_does_nothing() {
        let mut a = Automation::default();
        assert!(actions(a.tick(&Settings::default(), &status(99, 99, false), Some(false))).is_empty());
    }

    #[test]
    fn unplug_and_replug() {
        let mut a = Automation::default();
        let s = Settings { battery_quiet: true, battery_kbd_off: true, ..Default::default() };
        a.prime(Some(true));
        assert!(actions(a.tick(&s, &status(60, 55, false), Some(true))).is_empty());
        assert_eq!(
            actions(a.tick(&s, &status(60, 55, false), Some(false))),
            vec![Action::Fan(FanMode::Silent), Action::Kbd(0)]
        );
        assert!(actions(a.tick(&s, &status(60, 55, false), Some(false))).is_empty());
        assert_eq!(actions(a.tick(&s, &status(60, 55, false), Some(true))), vec![Action::RestoreUser]);
    }

    #[test]
    fn starting_on_battery_applies_battery_profile() {
        let mut a = Automation::default();
        let s = Settings { battery_quiet: true, ..Default::default() };
        a.prime(Some(false));
        assert_eq!(actions(a.tick(&s, &status(60, 55, false), Some(false))), vec![Action::Fan(FanMode::Silent)]);
    }
}
