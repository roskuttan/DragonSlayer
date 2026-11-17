//! High-level, safe control of the laptop.
//!
//! Every write goes through `guarded_write`, which enforces:
//!   1. the EC firmware is a verified model (else: no writes at all),
//!   2. the address is on the `WRITABLE` allowlist,
//!   3. the value is a known-valid value for that address,
//!   4. the value reads back correctly after writing.

use std::fmt;

use crate::ec::backend::{EcBackend, EcError, EcResult};
use crate::ec::map::{self, ChargeLimit, FanCurve, FanMode, KbdBacklight, ShiftMode};

/// A snapshot of everything we can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub firmware: String,
    pub firmware_verified: bool,
    pub cpu_temp_c: u8,
    pub cpu_fan_pct: u8,
    pub gpu_temp_c: u8,
    pub gpu_fan_pct: u8,
    /// The one fan's real speed (tachometer), 0 = stopped.
    pub fan_rpm: u16,
    pub cooler_boost: bool,
    pub shift_mode: Result<ShiftMode, u8>,
    pub fan_mode: Result<FanMode, u8>,
    pub charge_limit: Result<ChargeLimit, u8>,
    pub kbd_backlight: Result<KbdBacklight, u8>,
    pub webcam_on: bool,
    pub fn_win_swapped: bool,
    /// Fan curve tables as the EC holds them right now.
    pub cpu_curve_temps: [u8; map::CURVE_STEPS],
    pub cpu_curve: [u8; map::CURVE_POINTS],
    pub gpu_curve_temps: [u8; map::CURVE_STEPS],
    pub gpu_curve: [u8; map::CURVE_POINTS],
}

fn show<T: fmt::Debug>(r: &Result<T, u8>) -> String {
    match r {
        Ok(v) => format!("{v:?}"),
        Err(raw) => format!("unknown (raw 0x{raw:02X})"),
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verified = if self.firmware_verified { "verified" } else { "NOT verified - writes disabled" };
        writeln!(f, "EC firmware   : {} ({verified})", self.firmware)?;
        writeln!(f, "CPU           : {} C, fan {}%", self.cpu_temp_c, self.cpu_fan_pct)?;
        writeln!(f, "GPU           : {} C, fan {}%", self.gpu_temp_c, self.gpu_fan_pct)?;
        writeln!(f, "Fan speed     : {} RPM", self.fan_rpm)?;
        // 0x80 is what this laptop reports when Dragon Center is in charge:
        // it never touches the shift-mode register (see docs, watch test).
        match &self.shift_mode {
            Err(0x80) => writeln!(f, "Shift mode    : not set (raw 0x80)")?,
            other => writeln!(f, "Shift mode    : {}", show(other))?,
        }
        writeln!(f, "Fan mode      : {}", show(&self.fan_mode))?;
        writeln!(f, "Cooler Boost  : {}", if self.cooler_boost { "ON" } else { "off" })?;
        let charge = match &self.charge_limit {
            Ok(c) => format!("stop at {}%", c.percent()),
            Err(raw) => format!("not set (raw 0x{raw:02X})"),
        };
        writeln!(f, "Battery limit : {charge}")?;
        let kbd = match &self.kbd_backlight {
            Ok(k) => format!("level {}/{}", k.level(), KbdBacklight::MAX),
            Err(raw) => format!("unknown (raw 0x{raw:02X})"),
        };
        writeln!(f, "Keyboard light: {kbd}")?;
        writeln!(f, "Webcam        : {}", if self.webcam_on { "on" } else { "off" })?;
        writeln!(f, "Fn/Win swap   : {}", if self.fn_win_swapped { "swapped" } else { "normal" })?;
        writeln!(f, "CPU fan curve : {:?} % at {:?} C", self.cpu_curve, self.cpu_curve_temps)?;
        write!(f, "GPU fan curve : {:?} % at {:?} C", self.gpu_curve, self.gpu_curve_temps)
    }
}

pub struct Controller<B: EcBackend> {
    ec: B,
    firmware: String,
    verified: bool,
}

impl<B: EcBackend> Controller<B> {
    /// Connects and reads the EC firmware string to decide if writes are allowed.
    pub fn new(mut ec: B) -> EcResult<Self> {
        let raw = ec.read_range(map::FW_VERSION_ADDR, map::FW_VERSION_LEN)?;
        let firmware: String = raw
            .iter()
            .take_while(|&&b| b != 0)
            .filter(|b| b.is_ascii_graphic() || **b == b' ')
            .map(|&b| b as char)
            .collect::<String>()
            .trim()
            .to_string();
        let verified = map::firmware_supported(&firmware);
        Ok(Self { ec, firmware, verified })
    }

    pub fn firmware(&self) -> &str {
        &self.firmware
    }

    pub fn is_verified(&self) -> bool {
        self.verified
    }

    /// True if settings can be changed (verified firmware + writable backend).
    pub fn can_write(&self) -> bool {
        self.verified && self.ec.can_write()
    }

    pub fn into_backend(self) -> B {
        self.ec
    }

    /// All 256 EC bytes, for checking the register map against reality.
    pub fn dump(&mut self) -> EcResult<Vec<u8>> {
        self.ec.read_range(0, 256)
    }

    pub fn status(&mut self) -> EcResult<Status> {
        let r = |ec: &mut B, a| ec.read(a);
        let shift = r(&mut self.ec, map::SHIFT_MODE)?;
        let fan = r(&mut self.ec, map::FAN_MODE)?;
        let charge = r(&mut self.ec, map::CHARGE_LIMIT)?;
        let kbd = r(&mut self.ec, map::KBD_BACKLIGHT)?;
        let mut table = |start: u8, len: usize| self.ec.read_range(start, len);
        let cpu_curve_temps = arr(table(map::CPU_CURVE_TEMPS, map::CURVE_STEPS)?);
        let cpu_curve = arr(table(map::CPU_CURVE_SPEEDS, map::CURVE_POINTS)?);
        let gpu_curve_temps = arr(table(map::GPU_CURVE_TEMPS, map::CURVE_STEPS)?);
        let gpu_curve = arr(table(map::GPU_CURVE_SPEEDS, map::CURVE_POINTS)?);
        let tach = table(map::FAN_TACH, 2)?;
        Ok(Status {
            fan_rpm: map::fan_rpm(tach[0], tach[1]),
            cpu_curve_temps,
            cpu_curve,
            gpu_curve_temps,
            gpu_curve,
            firmware: self.firmware.clone(),
            firmware_verified: self.verified,
            cpu_temp_c: r(&mut self.ec, map::CPU_TEMP)?,
            cpu_fan_pct: r(&mut self.ec, map::CPU_FAN_SPEED)?,
            gpu_temp_c: r(&mut self.ec, map::GPU_TEMP)?,
            gpu_fan_pct: r(&mut self.ec, map::GPU_FAN_SPEED)?,
            cooler_boost: map::bit(r(&mut self.ec, map::COOLER_BOOST)?, map::COOLER_BOOST_BIT),
            shift_mode: ShiftMode::from_byte(shift).ok_or(shift),
            fan_mode: FanMode::from_byte(fan).ok_or(fan),
            charge_limit: ChargeLimit::from_byte(charge).ok_or(charge),
            kbd_backlight: KbdBacklight::from_byte(kbd).ok_or(kbd),
            webcam_on: map::bit(r(&mut self.ec, map::WEBCAM)?, map::WEBCAM_BIT),
            fn_win_swapped: !map::bit(r(&mut self.ec, map::FN_WIN_SWAP)?, map::FN_WIN_SWAP_BIT),
        })
    }

    pub fn set_shift_mode(&mut self, m: ShiftMode) -> EcResult<()> {
        self.guarded_write(map::SHIFT_MODE, m.to_byte())
    }

    /// Raw fan-mode byte only. The app uses `set_fan_profile` instead.
    pub fn set_fan_mode(&mut self, m: FanMode) -> EcResult<()> {
        self.guarded_write(map::FAN_MODE, m.to_byte())
    }

    /// Fan mode plus the matching curve: `Advanced` gets `curve` (factory if
    /// None); every other mode gets the factory curve back first, so "Auto"
    /// really means MSI's own behaviour.
    pub fn set_fan_profile(&mut self, m: FanMode, curve: Option<FanCurve>) -> EcResult<()> {
        let c = if m == FanMode::Advanced { curve.unwrap_or(FanCurve::FACTORY) } else { FanCurve::FACTORY };
        self.write_curve(c)?;
        self.set_fan_mode(m)
    }

    /// Writes the 7 speeds to both the CPU and GPU tables (only bytes that differ).
    fn write_curve(&mut self, c: FanCurve) -> EcResult<()> {
        for base in [map::CPU_CURVE_SPEEDS, map::GPU_CURVE_SPEEDS] {
            for (i, &v) in c.speeds().iter().enumerate() {
                let addr = base + i as u8;
                if self.ec.read(addr)? != v {
                    self.guarded_write(addr, v)?;
                }
            }
        }
        Ok(())
    }

    /// Webcam power (bit 1 of 0x2E); every other bit in that byte is preserved.
    pub fn set_webcam(&mut self, on: bool) -> EcResult<()> {
        self.set_bit(map::WEBCAM, map::WEBCAM_BIT, on)
    }

    pub fn set_charge_limit(&mut self, c: ChargeLimit) -> EcResult<()> {
        self.guarded_write(map::CHARGE_LIMIT, c.to_byte())
    }

    pub fn set_kbd_backlight(&mut self, k: KbdBacklight) -> EcResult<()> {
        self.guarded_write(map::KBD_BACKLIGHT, k.to_byte())
    }

    /// Flips only bit 7 of 0x98; every other bit in that byte is preserved.
    pub fn set_cooler_boost(&mut self, on: bool) -> EcResult<()> {
        self.set_bit(map::COOLER_BOOST, map::COOLER_BOOST_BIT, on)
    }

    fn set_bit(&mut self, addr: u8, bit: u8, on: bool) -> EcResult<()> {
        self.require_verified()?;
        let current = self.ec.read(addr)?;
        let next = map::with_bit(current, bit, on);
        if next == current {
            return Ok(());
        }
        self.guarded_write(addr, next)
    }

    fn require_verified(&self) -> EcResult<()> {
        if self.verified { Ok(()) } else { Err(EcError::UnsupportedFirmware(self.firmware.clone())) }
    }

    fn value_is_valid(&mut self, addr: u8, value: u8) -> EcResult<bool> {
        if let Some(bit) = bit_register(addr) {
            // Only that one bit may differ from what's there now.
            let current = self.ec.read(addr)?;
            return Ok((current ^ value) & !(1 << bit) == 0);
        }
        if map::is_curve_speed(addr) {
            return Ok((FanCurve::MIN_PCT..=FanCurve::MAX_PCT).contains(&value));
        }
        Ok(match addr {
            map::SHIFT_MODE => ShiftMode::from_byte(value).is_some(),
            map::FAN_MODE => FanMode::from_byte(value).is_some(),
            map::CHARGE_LIMIT => ChargeLimit::from_byte(value).is_some(),
            map::KBD_BACKLIGHT => KbdBacklight::from_byte(value).is_some(),
            _ => false,
        })
    }

    fn guarded_write(&mut self, addr: u8, value: u8) -> EcResult<()> {
        self.require_verified()?;
        if !map::is_writable(addr) {
            return Err(EcError::NotWritable(addr));
        }
        if !self.value_is_valid(addr, value)? {
            return Err(EcError::InvalidValue { addr, value });
        }
        self.ec.write(addr, value)?;
        let read = self.ec.read(addr)?;
        // For one-bit settings the EC may flip unrelated bits on its own.
        let mask = bit_register(addr).map_or(0xFF, |b| 1u8 << b);
        if (read ^ value) & mask != 0 {
            return Err(EcError::VerifyFailed { addr, wrote: value, read });
        }
        Ok(())
    }
}

/// Registers where the app changes exactly one bit.
fn bit_register(addr: u8) -> Option<u8> {
    match addr {
        map::COOLER_BOOST => Some(map::COOLER_BOOST_BIT),
        map::WEBCAM => Some(map::WEBCAM_BIT),
        _ => None,
    }
}

fn arr<const N: usize>(v: Vec<u8>) -> [u8; N] {
    let mut a = [0u8; N];
    a.copy_from_slice(&v[..N]);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::backend::MockEc;

    fn ctl() -> Controller<MockEc> {
        Controller::new(MockEc::gf63_idle()).unwrap()
    }

    #[test]
    fn reads_firmware_and_verifies() {
        let c = ctl();
        assert_eq!(c.firmware(), "16R5EMS1.104");
        assert!(c.is_verified());
    }

    #[test]
    fn status_decodes_mock() {
        let s = ctl().status().unwrap();
        assert_eq!(s.cpu_temp_c, 52);
        assert_eq!(s.shift_mode, Ok(ShiftMode::Comfort));
        assert_eq!(s.fan_mode, Ok(FanMode::Auto));
        assert_eq!(s.charge_limit.map(|c| c.percent()), Ok(100));
        assert_eq!(s.kbd_backlight.map(|k| k.level()), Ok(2));
        assert!(!s.cooler_boost);
        assert!(s.webcam_on);
    }

    #[test]
    fn setters_write_expected_bytes() {
        let mut c = ctl();
        c.set_charge_limit(ChargeLimit::new(60).unwrap()).unwrap();
        c.set_kbd_backlight(KbdBacklight::new(0).unwrap()).unwrap();
        c.set_fan_mode(FanMode::Silent).unwrap();
        let ec = c.into_backend();
        assert_eq!(
            ec.write_log,
            vec![(map::CHARGE_LIMIT, 0xBC), (map::KBD_BACKLIGHT, 0x80), (map::FAN_MODE, 0x1D)]
        );
    }

    #[test]
    fn shift_mode_writes_are_blocked() {
        let mut c = ctl();
        assert_eq!(c.set_shift_mode(ShiftMode::Turbo), Err(EcError::NotWritable(map::SHIFT_MODE)));
        assert!(c.into_backend().write_log.is_empty());
    }

    #[test]
    fn cooler_boost_preserves_other_bits() {
        let mut c = ctl(); // 0x98 starts at 0x02
        c.set_cooler_boost(true).unwrap();
        assert_eq!(c.into_backend().regs[map::COOLER_BOOST as usize], 0x82);
    }

    #[test]
    fn cooler_boost_noop_writes_nothing() {
        let mut c = ctl();
        c.set_cooler_boost(false).unwrap(); // already off
        assert!(c.into_backend().write_log.is_empty());
    }

    #[test]
    fn unknown_firmware_blocks_every_write() {
        let mut c = Controller::new(MockEc::gf63_idle().with_firmware("17F2EMS1.106")).unwrap();
        assert!(!c.is_verified());
        assert!(matches!(c.set_shift_mode(ShiftMode::Turbo), Err(EcError::UnsupportedFirmware(_))));
        assert!(matches!(c.set_cooler_boost(true), Err(EcError::UnsupportedFirmware(_))));
        assert!(c.status().is_ok(), "reading is still allowed");
        assert!(c.into_backend().write_log.is_empty());
    }

    #[test]
    fn non_allowlisted_address_is_refused() {
        let mut c = ctl();
        assert_eq!(c.guarded_write(map::CPU_TEMP, 10), Err(EcError::NotWritable(map::CPU_TEMP)));
        assert_eq!(c.guarded_write(map::CPU_CURVE_TEMPS, 60), Err(EcError::NotWritable(map::CPU_CURVE_TEMPS)));
        assert_eq!(c.guarded_write(map::FN_WIN_SWAP, 0), Err(EcError::NotWritable(map::FN_WIN_SWAP)));
        assert!(c.into_backend().write_log.is_empty());
    }

    #[test]
    fn webcam_flips_only_its_bit() {
        let mut c = ctl(); // 0x2E starts at 0x02 (on)
        c.set_webcam(false).unwrap();
        c.set_webcam(false).unwrap(); // no-op
        assert!(!c.status().unwrap().webcam_on);
        c.set_webcam(true).unwrap();
        let ec = c.into_backend();
        assert_eq!(ec.write_log, vec![(map::WEBCAM, 0x00), (map::WEBCAM, 0x02)]);
        // a webcam byte with another bit changed is refused
        let mut c = ctl();
        assert!(matches!(c.guarded_write(map::WEBCAM, 0x03), Err(EcError::InvalidValue { .. })));
    }

    #[test]
    fn custom_curve_writes_both_tables_then_mode() {
        let mut c = ctl();
        c.set_fan_profile(FanMode::Advanced, Some(FanCurve::COOL)).unwrap();
        let st = c.status().unwrap();
        assert_eq!(st.cpu_curve, FanCurve::COOL.speeds());
        assert_eq!(st.gpu_curve, FanCurve::COOL.speeds());
        assert_eq!(st.fan_mode, Ok(FanMode::Advanced));
        assert_eq!(st.cpu_curve_temps, [55, 64, 73, 76, 82, 88], "temperature steps untouched");
        let ec = c.into_backend();
        assert_eq!(ec.write_log.last(), Some(&(map::FAN_MODE, 0x8D)), "mode is written last");
        assert!(ec.write_log.iter().all(|(a, _)| map::is_curve_speed(*a) || *a == map::FAN_MODE));
    }

    #[test]
    fn auto_restores_factory_curve() {
        let mut c = ctl();
        c.set_fan_profile(FanMode::Advanced, Some(FanCurve::MAX)).unwrap();
        c.set_fan_profile(FanMode::Auto, Some(FanCurve::MAX)).unwrap();
        let st = c.status().unwrap();
        assert_eq!(st.cpu_curve, FanCurve::FACTORY.speeds());
        assert_eq!(st.gpu_curve, FanCurve::FACTORY.speeds());
        assert_eq!(st.fan_mode, Ok(FanMode::Auto));
    }

    #[test]
    fn factory_profile_on_factory_ec_writes_only_the_mode() {
        let mut c = ctl();
        c.set_fan_profile(FanMode::Silent, None).unwrap();
        assert_eq!(c.into_backend().write_log, vec![(map::FAN_MODE, 0x1D)]);
    }

    #[test]
    fn curve_bytes_outside_limits_are_refused() {
        let mut c = ctl();
        assert!(matches!(c.guarded_write(map::CPU_CURVE_SPEEDS, 5), Err(EcError::InvalidValue { .. })));
        assert!(matches!(c.guarded_write(map::GPU_CURVE_SPEEDS + 6, 150), Err(EcError::InvalidValue { .. })));
        assert!(c.into_backend().write_log.is_empty());
    }

    #[test]
    fn invalid_values_are_refused() {
        let mut c = ctl();
        assert!(matches!(c.guarded_write(map::FAN_MODE, 0x00), Err(EcError::InvalidValue { .. })));
        assert!(matches!(c.guarded_write(map::CHARGE_LIMIT, 0x3C), Err(EcError::InvalidValue { .. })));
        // Cooler boost byte with an extra bit changed besides bit 7.
        assert!(matches!(c.guarded_write(map::COOLER_BOOST, 0x83), Err(EcError::InvalidValue { .. })));
        assert!(c.into_backend().write_log.is_empty());
    }

    /// A backend whose writes silently don't stick.
    struct StuckEc(MockEc);
    impl EcBackend for StuckEc {
        fn read(&mut self, a: u8) -> EcResult<u8> {
            self.0.read(a)
        }
        fn write(&mut self, _a: u8, _v: u8) -> EcResult<()> {
            Ok(())
        }
    }

    #[test]
    fn write_that_does_not_stick_is_reported() {
        let mut c = Controller::new(StuckEc(MockEc::gf63_idle())).unwrap();
        let err = c.set_fan_mode(FanMode::Silent).unwrap_err();
        assert_eq!(err, EcError::VerifyFailed { addr: map::FAN_MODE, wrote: 0x1D, read: 0x0D });
    }
}
