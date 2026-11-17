//! Screen refresh rate: which rates the built-in screen offers, and
//! switching between them by situation (saves battery at the slow rate).
//!
//! This laptop's panel (BOE 0957) lists 48 and 144 Hz only, and it has no
//! variable refresh: every switch blanks the screen for a moment. So the
//! app switches rarely, at clear moments, never second by second. `pick`
//! works with whatever rates are listed, so in-between rates added later
//! as custom display modes get used without changes here.

use std::ffi::c_void;

use crate::settings::Refresh;

/// DEVMODEW (220 bytes), display part.
#[repr(C)]
struct DevMode {
    device_name: [u16; 32],
    spec_version: u16,
    driver_version: u16,
    size: u16,
    driver_extra: u16,
    fields: u32,
    position: [i32; 2],
    orientation: u32,
    fixed_output: u32,
    color: i16,
    duplex: i16,
    y_resolution: i16,
    tt_option: i16,
    collate: i16,
    form_name: [u16; 32],
    log_pixels: u16,
    bits_per_pel: u32,
    width: u32,
    height: u32,
    flags: u32,
    frequency: u32,
    rest: [u32; 8],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumDisplaySettingsW(device: *const u16, mode: u32, dm: *mut DevMode) -> i32;
    fn ChangeDisplaySettingsExW(device: *const u16, dm: *mut DevMode, hwnd: *mut c_void, flags: u32, param: *mut c_void) -> i32;
}

const ENUM_CURRENT_SETTINGS: u32 = u32::MAX;
const DM_PELSWIDTH: u32 = 0x0008_0000;
const DM_PELSHEIGHT: u32 = 0x0010_0000;
const DM_DISPLAYFREQUENCY: u32 = 0x0040_0000;
const CDS_TEST: u32 = 2;

fn blank() -> DevMode {
    // SAFETY: DevMode is plain integers; all-zero is a valid value.
    let mut dm: DevMode = unsafe { std::mem::zeroed() };
    dm.size = size_of::<DevMode>() as u16;
    dm
}

fn current() -> Option<DevMode> {
    let mut dm = blank();
    // SAFETY: valid out-pointer with its size field set.
    (unsafe { EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut dm) } != 0).then_some(dm)
}

/// The main screen's refresh rate right now, in Hz.
pub fn current_hz() -> Option<u32> {
    current().map(|d| d.frequency)
}

/// Every refresh rate offered at the current resolution, slowest first.
pub fn rates() -> Vec<u32> {
    let Some(cur) = current() else { return Vec::new() };
    let mut out = Vec::new();
    let mut dm = blank();
    let mut i = 0;
    // SAFETY: valid out-pointer; the loop ends when Windows has no more modes.
    while unsafe { EnumDisplaySettingsW(std::ptr::null(), i, &mut dm) } != 0 {
        if dm.width == cur.width && dm.height == cur.height && dm.bits_per_pel == cur.bits_per_pel && dm.frequency > 1 && !out.contains(&dm.frequency) {
            out.push(dm.frequency);
        }
        i += 1;
    }
    out.sort_unstable();
    out
}

/// Switches the main screen to `hz` (same resolution). Not saved as the
/// Windows default: after a restart Windows' own setting applies again.
pub fn set_hz(hz: u32) -> Result<(), String> {
    let mut dm = current().ok_or("can't read the display mode")?;
    if dm.frequency == hz {
        return Ok(());
    }
    dm.frequency = hz;
    dm.fields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    // SAFETY: valid DEVMODE; first a dry run, then the change itself.
    unsafe {
        let test = ChangeDisplaySettingsExW(std::ptr::null(), &mut dm, std::ptr::null_mut(), CDS_TEST, std::ptr::null_mut());
        if test != 0 {
            return Err(format!("the screen refuses {hz} Hz (code {test})"));
        }
        match ChangeDisplaySettingsExW(std::ptr::null(), &mut dm, std::ptr::null_mut(), 0, std::ptr::null_mut()) {
            0 => Ok(()),
            code => Err(format!("switching to {hz} Hz failed (code {code})")),
        }
    }
}

/// The rate for a situation, from the rates the screen offers (sorted).
/// - plugged in: the fastest
/// - on battery, heavy use (a game): the fastest rate up to 120 Hz if there
///   is one above the slowest, else the fastest
/// - on battery otherwise: the slowest rate of at least 60 Hz if that is
///   not also the fastest, else the slowest
pub fn pick(rates: &[u32], plugged: bool, heavy: bool) -> Option<u32> {
    let (&slow, &fast) = (rates.first()?, rates.last()?);
    Some(if plugged {
        fast
    } else if heavy {
        rates.iter().rev().copied().find(|&r| r <= 120 && r > slow).unwrap_or(fast)
    } else {
        rates.iter().copied().find(|&r| r >= 60 && r < fast).unwrap_or(slow)
    })
}

/// Decides when to switch: the wanted rate must hold for 5 s, and switches
/// are at least 20 s apart (each one blanks the screen briefly).
#[derive(Debug, Default)]
pub struct Switcher {
    rates: Vec<u32>,
    held_secs: f32,
    since_switch: Option<f32>,
}

impl Switcher {
    /// The rate to switch to now, if any. `current` is the live rate.
    pub fn decide(&mut self, policy: Refresh, rates: &[u32], current: u32, plugged: bool, heavy: bool, dt: f32) -> Option<u32> {
        self.rates = rates.to_vec();
        if let Some(s) = &mut self.since_switch {
            *s += dt;
        }
        let want = match policy {
            Refresh::Off => return None,
            Refresh::Battery => pick(rates, plugged, false),
            Refresh::Adaptive => pick(rates, plugged, heavy),
        }?;
        if want == current {
            self.held_secs = 0.0;
            return None;
        }
        self.held_secs += dt;
        if self.held_secs >= 5.0 && self.since_switch.is_none_or(|s| s >= 20.0) {
            self.held_secs = 0.0;
            self.since_switch = Some(0.0);
            return Some(want);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_situation() {
        // this laptop today
        let two = [48, 144];
        assert_eq!(pick(&two, true, false), Some(144));
        assert_eq!(pick(&two, false, false), Some(48));
        assert_eq!(pick(&two, false, true), Some(144), "no middle rate: a game gets the fastest");
        // with in-between rates added
        let many = [48, 60, 90, 120, 144];
        assert_eq!(pick(&many, true, true), Some(144));
        assert_eq!(pick(&many, false, false), Some(60), "light use on battery: slowest that is at least 60");
        assert_eq!(pick(&many, false, true), Some(120), "a game on battery: fast, but not the very fastest");
        assert_eq!(pick(&[60], false, true), Some(60));
        assert_eq!(pick(&[], true, false), None);
    }

    #[test]
    fn switches_rarely() {
        let rates = [48, 144];
        let mut s = Switcher::default();
        // unplugged: waits 5 s, then one switch
        let mut out = Vec::new();
        for _ in 0..8 {
            out.push(s.decide(Refresh::Battery, &rates, 144, false, false, 1.0));
        }
        assert_eq!(out.iter().flatten().count(), 1);
        assert_eq!(out[4], Some(48), "on the fifth second");
        // plugged back in 3 s later: not before 20 s have passed
        let mut waited = 0;
        while s.decide(Refresh::Battery, &rates, 48, true, false, 1.0).is_none() {
            waited += 1;
            assert!(waited < 60);
        }
        assert!(waited >= 16, "second switch came after only {waited} s");
        // a 3-second flicker of GPU load doesn't switch
        let mut s = Switcher::default();
        for heavy in [true, true, true, false, false, true, true, false] {
            assert_eq!(s.decide(Refresh::Adaptive, &rates, 48, false, heavy, 1.0), None);
        }
        // off never switches
        assert_eq!(Switcher::default().decide(Refresh::Off, &rates, 48, true, false, 100.0), None);
    }

    #[test]
    fn reads_the_real_screen() {
        // only checks that the calls work on whatever machine runs the test
        if let Some(hz) = current_hz() {
            assert!(rates().contains(&hz), "current rate {hz} is in the list {:?}", rates());
            assert_eq!(size_of::<DevMode>(), 220);
        }
    }
}
