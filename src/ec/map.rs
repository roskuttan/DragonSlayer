//! EC register map for the MSI GF63 Thin 10UC.
//!
//! Source: the open-source `msi-ec` Linux driver, config `CONF_G1_7`
//! (firmware family `16R5EMS1.1`). Cross-checked against MSI's own service
//! log on this laptop, which reads EC offsets 0xEF and 0x98 through the
//! memory window at 0xFC000800.
//!
//! This module is pure data + encode/decode. It never touches hardware.

/// EC firmware families this map is known to be correct for.
/// The driver compares the version string with the trailing update digits
/// dropped, e.g. "16R5EMS1.104" is matched as "16R5EMS1.1".
pub const SUPPORTED_FW_PREFIXES: &[&str] = &["16R5EMS1.1"];

/// Where the ASCII firmware version string lives (per msi-ec).
pub const FW_VERSION_ADDR: u8 = 0xA0;
pub const FW_VERSION_LEN: usize = 12;

// ---- read-only sensors ----------------------------------------------------
pub const CPU_TEMP: u8 = 0x68; // degrees C
pub const CPU_FAN_SPEED: u8 = 0x71; // raw percent
pub const GPU_TEMP: u8 = 0x80; // degrees C
pub const GPU_FAN_SPEED: u8 = 0x89; // raw percent
/// Fan tachometer, 16-bit big-endian (0xCC = high byte). The GF63 Thin has
/// ONE fan for CPU and GPU; this is its only tachometer. Verified 2026-11-16:
/// ~2,900 RPM in Auto, jumped to ~6,850 with Cooler Boost. (0xC8 jitters
/// but ignores Cooler Boost, 0xCA is always 0: neither is a fan.)
pub const FAN_TACH: u8 = 0xCC;

/// RPM from the tachometer reading (period count): 480000 / raw.
/// 0 (or a reading meaning < 100 RPM) is a stopped fan.
pub fn fan_rpm(hi: u8, lo: u8) -> u16 {
    let raw = u32::from(u16::from_be_bytes([hi, lo]));
    if raw == 0 {
        return 0;
    }
    let rpm = 480_000 / raw;
    if rpm < 100 { 0 } else { rpm.min(u32::from(u16::MAX)) as u16 }
}

// ---- controllable settings ------------------------------------------------
pub const COOLER_BOOST: u8 = 0x98;
pub const COOLER_BOOST_BIT: u8 = 7;

/// UNVERIFIED on this laptop: reads 0x80 and Dragon Center never writes it.
pub const SHIFT_MODE: u8 = 0xF2;
/// Verified by watching Dragon Center (Auto 0x0D, Silent 0x1D).
pub const FAN_MODE: u8 = 0xF4;
pub const CHARGE_LIMIT: u8 = 0xEF;
pub const KBD_BACKLIGHT: u8 = 0xF3;

/// Webcam power: bit 1 set = camera on (msi-ec). On this laptop it read 0
/// while Windows saw no camera at all.
pub const WEBCAM: u8 = 0x2E;
pub const WEBCAM_BIT: u8 = 1;

// ---- fan curve ("Advanced" fan mode) --------------------------------------
// Layout read live on 2026-11-16: 6 temperature steps + 7 fan speeds per fan.
// Speed i applies once the temperature reaches step i-1 (speed 0 below step 0).
// The live fan % (0x71 / 0x89) matched this table exactly in Auto mode.
pub const CPU_CURVE_TEMPS: u8 = 0x6A;
pub const CPU_CURVE_SPEEDS: u8 = 0x72;
pub const GPU_CURVE_TEMPS: u8 = 0x82;
pub const GPU_CURVE_SPEEDS: u8 = 0x8A;
pub const CURVE_STEPS: usize = 6;
pub const CURVE_POINTS: usize = 7;

// ---- read-only (writes not exposed) ---------------------------------------
pub const FN_WIN_SWAP: u8 = 0xBF;
pub const FN_WIN_SWAP_BIT: u8 = 4; // inverted: bit set = NOT swapped (to verify on hardware)

/// Single-byte settings the app may write. Anything else is refused.
/// SHIFT_MODE is deliberately absent: unverified on this laptop.
pub const WRITABLE: &[u8] = &[COOLER_BOOST, FAN_MODE, CHARGE_LIMIT, KBD_BACKLIGHT, WEBCAM];

/// True if `addr` is one of the 14 fan-speed bytes (never the temperature steps).
pub fn is_curve_speed(addr: u8) -> bool {
    let n = CURVE_POINTS as u8;
    (CPU_CURVE_SPEEDS..CPU_CURVE_SPEEDS + n).contains(&addr) || (GPU_CURVE_SPEEDS..GPU_CURVE_SPEEDS + n).contains(&addr)
}

/// Every address the app is ever allowed to write.
pub fn is_writable(addr: u8) -> bool {
    WRITABLE.contains(&addr) || is_curve_speed(addr)
}

/// Returns true if `fw` (as read from the EC) belongs to a supported family.
pub fn firmware_supported(fw: &str) -> bool {
    let fw = fw.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    SUPPORTED_FW_PREFIXES.iter().any(|p| fw.starts_with(p))
}

/// Performance profile ("shift mode" in MSI terms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftMode {
    Eco,
    Comfort,
    Sport,
    Turbo,
}

impl ShiftMode {
    pub const ALL: [ShiftMode; 4] = [Self::Eco, Self::Comfort, Self::Sport, Self::Turbo];

    pub fn to_byte(self) -> u8 {
        match self {
            Self::Eco => 0xC2,
            Self::Comfort => 0xC1,
            Self::Sport => 0xC0,
            Self::Turbo => 0xC4,
        }
    }

    pub fn from_byte(b: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.to_byte() == b)
    }
}

/// Fan control policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    Auto,
    Silent,
    Basic,
    Advanced,
}

impl FanMode {
    pub const ALL: [FanMode; 4] = [Self::Auto, Self::Silent, Self::Basic, Self::Advanced];

    pub fn to_byte(self) -> u8 {
        match self {
            Self::Auto => 0x0D,
            Self::Silent => 0x1D,
            Self::Basic => 0x4D,
            Self::Advanced => 0x8D,
        }
    }

    pub fn from_byte(b: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.to_byte() == b)
    }
}

/// Battery "stop charging at N%" setting. Valid range 10..=100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargeLimit(u8);

impl ChargeLimit {
    pub const MIN: u8 = 10;
    pub const MAX: u8 = 100;
    const ENABLED_FLAG: u8 = 0x80;

    pub fn new(percent: u8) -> Option<Self> {
        (Self::MIN..=Self::MAX).contains(&percent).then_some(Self(percent))
    }

    pub fn percent(self) -> u8 {
        self.0
    }

    /// Stored as bit 7 set + percent in the low 7 bits. 60% -> 0xBC.
    pub fn to_byte(self) -> u8 {
        Self::ENABLED_FLAG | self.0
    }

    /// `None` if the flag bit is clear or the value is out of range.
    pub fn from_byte(b: u8) -> Option<Self> {
        if b & Self::ENABLED_FLAG == 0 {
            return None;
        }
        Self::new(b & !Self::ENABLED_FLAG)
    }
}

/// Keyboard backlight level 0 (off) ..= 3 (brightest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KbdBacklight(u8);

impl KbdBacklight {
    pub const MAX: u8 = 3;
    const BASE: u8 = 0x80;

    pub fn new(level: u8) -> Option<Self> {
        (level <= Self::MAX).then_some(Self(level))
    }

    pub fn level(self) -> u8 {
        self.0
    }

    pub fn to_byte(self) -> u8 {
        Self::BASE | self.0
    }

    pub fn from_byte(b: u8) -> Option<Self> {
        if b & Self::BASE == 0 {
            return None;
        }
        Self::new(b & !Self::BASE)
    }
}

/// Fan speeds (percent) for the 7 points of a fan curve.
///
/// Safety rules, enforced by `new`:
/// - every point between `MIN_PCT` and 100,
/// - never decreasing (hotter never means slower),
/// - the two hottest points never below factory, so cooling under heavy
///   load is at least as strong as MSI's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanCurve([u8; CURVE_POINTS]);

impl FanCurve {
    pub const MIN_PCT: u8 = 20;
    pub const MAX_PCT: u8 = 100;
    /// Read from this laptop's EC in Auto mode (same for CPU and GPU).
    pub const FACTORY: FanCurve = FanCurve([38, 43, 48, 54, 60, 70, 85]);

    // Quiet / Cool / Max are tuned from a measurement of this laptop
    // (research/measure-fan.ps1, 2026-11-17; docs/how-dragon-center-works.md):
    // - RPM is close to 59.5 x percent (35 % = 2,080, 50 % = 2,990,
    //   100 % = 5,770; Cooler Boost goes beyond the curve: 6,860).
    // - In light use the CPU sits at 62-66 C whatever the fan does: 50 %,
    //   65 % and 90 % differ by about 1 C and 5 C. So extra speed below 73 C
    //   is mostly noise.
    // - 62-66 C straddles the 64 C step, so a curve that changes there makes
    //   the fan hunt up and down. Each preset holds ONE speed from 55 to 73 C.
    // - Load pushes the CPU up 20 C within seconds, while the fan needs
    //   about 25 s to go from 2,100 to 4,200 RPM. Starting a burst at 35 %
    //   the CPU peaked at 92 C. So every preset is already ramping at 73 C
    //   and 76 C instead of waiting for the hot end.
    /// Light use at about 1,960 RPM (factory: 2,560-2,860), then up early.
    pub const QUIET: FanCurve = FanCurve([30, 33, 33, 45, 58, 70, 85]);
    /// Spinning faster before load arrives, to cut the peaks.
    pub const COOL: FanCurve = FanCurve([45, 52, 52, 65, 75, 85, 95]);
    /// As much as the curve allows: 100 % from 82 C.
    pub const MAX: FanCurve = FanCurve([60, 66, 66, 80, 90, 100, 100]);

    /// Measured on this laptop: fan RPM per percent of fan speed.
    pub const RPM_PER_PCT: f32 = 59.5;

    pub fn new(speeds: [u8; CURVE_POINTS]) -> Option<Self> {
        let in_range = speeds.iter().all(|s| (Self::MIN_PCT..=Self::MAX_PCT).contains(s));
        let rising = speeds.windows(2).all(|w| w[0] <= w[1]);
        let f = Self::FACTORY.0;
        let strong_top = (CURVE_POINTS - 2..CURVE_POINTS).all(|i| speeds[i] >= f[i]);
        (in_range && rising && strong_top).then_some(Self(speeds))
    }

    pub fn speeds(self) -> [u8; CURVE_POINTS] {
        self.0
    }

    /// Lowest allowed value for point `i` (the floor for the hottest two).
    pub fn floor(i: usize) -> u8 {
        if i >= CURVE_POINTS - 2 { Self::FACTORY.0[i] } else { Self::MIN_PCT }
    }

    /// "a,b,c,d,e,f,g"
    pub fn to_text(self) -> String {
        self.0.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
    }

    pub fn parse(s: &str) -> Option<Self> {
        let v: Vec<u8> = s.split(',').map(|x| x.trim().parse().ok()).collect::<Option<_>>()?;
        Self::new(v.try_into().ok()?)
    }
}

/// True if `bit` is set in `byte`.
pub fn bit(byte: u8, bit: u8) -> bool {
    byte & (1 << bit) != 0
}

/// `byte` with `bit` set or cleared, all other bits untouched.
pub fn with_bit(byte: u8, bit: u8, on: bool) -> u8 {
    if on { byte | (1 << bit) } else { byte & !(1 << bit) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_mode_round_trips() {
        for m in ShiftMode::ALL {
            assert_eq!(ShiftMode::from_byte(m.to_byte()), Some(m));
        }
        assert_eq!(ShiftMode::Eco.to_byte(), 0xC2);
        assert_eq!(ShiftMode::Turbo.to_byte(), 0xC4);
        assert_eq!(ShiftMode::from_byte(0x00), None);
    }

    #[test]
    fn fan_mode_round_trips() {
        for m in FanMode::ALL {
            assert_eq!(FanMode::from_byte(m.to_byte()), Some(m));
        }
        assert_eq!(FanMode::from_byte(0xFF), None);
    }

    #[test]
    fn charge_limit_encoding() {
        assert_eq!(ChargeLimit::new(60).unwrap().to_byte(), 0xBC);
        assert_eq!(ChargeLimit::new(80).unwrap().to_byte(), 0xD0);
        assert_eq!(ChargeLimit::new(100).unwrap().to_byte(), 0xE4);
        assert_eq!(ChargeLimit::from_byte(0xBC).map(|c| c.percent()), Some(60));
        assert_eq!(ChargeLimit::new(9), None);
        assert_eq!(ChargeLimit::new(101), None);
        assert_eq!(ChargeLimit::from_byte(0x3C), None, "flag bit clear");
        assert_eq!(ChargeLimit::from_byte(0x85), None, "5% is out of range");
    }

    #[test]
    fn kbd_backlight_encoding() {
        assert_eq!(KbdBacklight::new(0).unwrap().to_byte(), 0x80);
        assert_eq!(KbdBacklight::new(3).unwrap().to_byte(), 0x83);
        assert_eq!(KbdBacklight::new(4), None);
        assert_eq!(KbdBacklight::from_byte(0x82).map(|k| k.level()), Some(2));
        assert_eq!(KbdBacklight::from_byte(0x02), None);
    }

    #[test]
    fn bit_helpers_touch_only_one_bit() {
        assert_eq!(with_bit(0b0000_0001, 7, true), 0b1000_0001);
        assert_eq!(with_bit(0b1111_1111, 7, false), 0b0111_1111);
        assert!(bit(0x80, 7));
        assert!(!bit(0x7F, 7));
    }

    #[test]
    fn firmware_matching() {
        assert!(firmware_supported("16R5EMS1.104"));
        assert!(firmware_supported("16R5EMS1.1\0\0"));
        assert!(!firmware_supported("16R5EMS2.104"));
        assert!(!firmware_supported("17F2EMS1.1"));
        assert!(!firmware_supported(""));
    }

    #[test]
    fn writable_list_is_exactly_the_settings() {
        assert!(is_writable(CHARGE_LIMIT));
        assert!(is_writable(WEBCAM));
        assert!(!is_writable(SHIFT_MODE), "unverified on this laptop");
        assert!(!is_writable(CPU_TEMP));
        assert!(!is_writable(FAN_TACH) && !is_writable(FAN_TACH + 1), "tachometer is read-only");
        assert!(!is_writable(CPU_FAN_SPEED), "live fan reading, not the curve");
        assert!(!is_writable(GPU_FAN_SPEED));
        assert!(!is_writable(FW_VERSION_ADDR));
        assert!(!is_writable(FN_WIN_SWAP));
        for i in 0..CURVE_STEPS as u8 {
            assert!(!is_writable(CPU_CURVE_TEMPS + i), "temperature steps stay factory");
            assert!(!is_writable(GPU_CURVE_TEMPS + i));
        }
        for i in 0..CURVE_POINTS as u8 {
            assert!(is_writable(CPU_CURVE_SPEEDS + i));
            assert!(is_writable(GPU_CURVE_SPEEDS + i));
        }
        assert!(!is_writable(CPU_CURVE_SPEEDS + CURVE_POINTS as u8));
        assert!(!is_writable(GPU_CURVE_SPEEDS + CURVE_POINTS as u8), "0x91");
    }

    #[test]
    fn fan_rpm_from_tachometer() {
        // readings taken on the real laptop
        assert_eq!(fan_rpm(0x00, 165), 2909); // Auto
        assert_eq!(fan_rpm(0x00, 70), 6857); // Cooler Boost
        assert_eq!(fan_rpm(0, 0), 0, "0 = stopped");
        assert_eq!(fan_rpm(0xFF, 0xFF), 0, "near-infinite period = stopped");
    }

    #[test]
    fn fan_curve_rules() {
        for c in [FanCurve::FACTORY, FanCurve::QUIET, FanCurve::COOL, FanCurve::MAX] {
            assert_eq!(FanCurve::new(c.speeds()), Some(c), "presets must pass the rules");
        }
        // presets: each step up in name is at least as fast at every point,
        // and none changes speed at the 64 C step this laptop idles on
        let order = [FanCurve::QUIET, FanCurve::FACTORY, FanCurve::COOL, FanCurve::MAX];
        for w in order.windows(2) {
            assert!(w[0].speeds().iter().zip(w[1].speeds()).all(|(a, b)| *a <= b), "{w:?}");
        }
        for c in [FanCurve::QUIET, FanCurve::COOL, FanCurve::MAX] {
            assert_eq!(c.speeds()[1], c.speeds()[2], "no hunting around 64 C");
        }
        assert_eq!(FanCurve::new([19, 43, 48, 54, 60, 70, 85]), None, "below minimum");
        assert_eq!(FanCurve::new([38, 43, 48, 54, 60, 70, 101]), None, "above 100");
        assert_eq!(FanCurve::new([38, 50, 48, 54, 60, 70, 85]), None, "decreasing");
        assert_eq!(FanCurve::new([38, 43, 48, 54, 60, 69, 85]), None, "hot point below factory");
        assert_eq!(FanCurve::new([38, 43, 48, 54, 60, 70, 84]), None, "top point below factory");
        assert_eq!(FanCurve::parse("38, 43,48,54,60,70,85"), Some(FanCurve::FACTORY));
        assert_eq!(FanCurve::parse(&FanCurve::COOL.to_text()), Some(FanCurve::COOL));
        assert_eq!(FanCurve::parse("38,43,48"), None);
        assert_eq!(FanCurve::parse("x"), None);
    }
}
