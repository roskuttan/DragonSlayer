//! Windows power plans (Balanced, Ultimate Performance, ...), switched with
//! the documented powrprof.dll calls `powercfg` uses.
//!
//! (The Windows 11 "Power mode" slider is not used: it only works on the
//! Balanced plan, and this laptop runs Ultimate Performance.)

use std::ffi::c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

const BALANCED: Guid = Guid { d1: 0x381B_4222, d2: 0xF694, d3: 0x41F0, d4: [0x96, 0x85, 0xFF, 0x5B, 0xB2, 0x60, 0xDF, 0x2E] };
const HIGH_PERFORMANCE: Guid = Guid { d1: 0x8C5E_7FDA, d2: 0xE8BF, d3: 0x4A96, d4: [0x9A, 0x85, 0xA6, 0xE2, 0x3A, 0x8C, 0x63, 0x5C] };
const POWER_SAVER: Guid = Guid { d1: 0xA184_1308, d2: 0x3541, d3: 0x4FAB, d4: [0xBC, 0x81, 0xF7, 0x15, 0x56, 0xF2, 0x0B, 0x4A] };

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub guid: Guid,
    pub name: String,
}

impl Plan {
    /// Short label for buttons: "Ultimate Performance" -> "Ultimate".
    pub fn short(&self) -> String {
        let n = self.name.as_str();
        let l = n.to_lowercase();
        if l.contains("ultimate") {
            "Ultimate".into()
        } else if l.contains("high perf") {
            "High perf.".into()
        } else if l.contains("saver") {
            "Saver".into()
        } else if n.chars().count() > 12 {
            n.chars().take(11).collect::<String>() + "…"
        } else {
            n.into()
        }
    }
}

#[link(name = "powrprof")]
unsafe extern "system" {
    fn PowerEnumerate(root: *mut c_void, scheme: *const Guid, sub: *const Guid, flags: u32, index: u32, buf: *mut u8, size: *mut u32) -> u32;
    fn PowerReadFriendlyName(root: *mut c_void, scheme: *const Guid, sub: *const Guid, setting: *const Guid, buf: *mut u8, size: *mut u32) -> u32;
    fn PowerGetActiveScheme(root: *mut c_void, active: *mut *mut Guid) -> u32;
    fn PowerSetActiveScheme(root: *mut c_void, scheme: *const Guid) -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

const ACCESS_SCHEME: u32 = 16;

fn friendly_name(g: &Guid) -> String {
    let read = |buf: *mut u8, size: &mut u32| {
        // SAFETY: `buf` is null (size query) or `*size` bytes long.
        unsafe { PowerReadFriendlyName(std::ptr::null_mut(), g, std::ptr::null(), std::ptr::null(), buf, size) }
    };
    let mut size = 0u32;
    let _ = read(std::ptr::null_mut(), &mut size);
    let mut buf = vec![0u16; (size as usize / 2).max(1) + 1];
    let mut size = (buf.len() * 2) as u32;
    let r = read(buf.as_mut_ptr().cast(), &mut size);
    if r != 0 {
        return format!("Unnamed plan (error {r})");
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Every power plan on this PC, in Windows' order.
pub fn plans() -> Vec<Plan> {
    let mut out = Vec::new();
    for index in 0..32 {
        let mut g = Guid::default();
        let mut size = std::mem::size_of::<Guid>() as u32;
        // SAFETY: the buffer is one GUID, as `size` says.
        let r = unsafe { PowerEnumerate(std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), ACCESS_SCHEME, index, (&mut g as *mut Guid).cast(), &mut size) };
        if r != 0 {
            break;
        }
        out.push(Plan { guid: g, name: friendly_name(&g) });
    }
    out
}

pub fn active() -> Option<Guid> {
    let mut p: *mut Guid = std::ptr::null_mut();
    // SAFETY: Windows allocates the GUID; we copy it and free it with LocalFree.
    unsafe {
        if PowerGetActiveScheme(std::ptr::null_mut(), &mut p) != 0 || p.is_null() {
            return None;
        }
        let g = *p;
        LocalFree(p.cast());
        Some(g)
    }
}

pub fn activate(g: &Guid) -> Result<(), String> {
    // SAFETY: pointer to a valid GUID.
    match unsafe { PowerSetActiveScheme(std::ptr::null_mut(), g) } {
        0 => Ok(()),
        e => Err(format!("Windows refused to switch power plan (error {e})")),
    }
}

/// Which plan to use for each scenario level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Saver,
    Balanced,
    Performance,
}

/// Picks the plan for `level` from `plans` (None if there's no fitting one).
pub fn pick(plans: &[Plan], level: Level) -> Option<&Plan> {
    let by_guid = |g: Guid| plans.iter().find(|p| p.guid == g);
    let by_name = |s: &str| plans.iter().find(|p| p.name.to_lowercase().contains(s));
    match level {
        Level::Saver => by_guid(POWER_SAVER).or_else(|| by_name("saver")),
        Level::Balanced => by_guid(BALANCED).or_else(|| by_name("balanced")),
        Level::Performance => by_name("ultimate").or_else(|| by_guid(HIGH_PERFORMANCE)).or_else(|| by_name("high perf")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(guid: Guid, name: &str) -> Plan {
        Plan { guid, name: name.into() }
    }

    #[test]
    fn picks_plans_by_guid_or_name() {
        let ultimate = Guid { d1: 0x85AB_914A, ..Guid::default() };
        let ps = vec![plan(BALANCED, "Balanced"), plan(ultimate, "Ultimate Performance"), plan(Guid { d1: 7, ..Guid::default() }, "Limit Power")];
        assert_eq!(pick(&ps, Level::Balanced).map(|p| p.guid), Some(BALANCED));
        assert_eq!(pick(&ps, Level::Performance).map(|p| p.guid), Some(ultimate));
        assert_eq!(pick(&ps, Level::Saver), None);
        assert_eq!(ps[1].short(), "Ultimate");
        assert_eq!(ps[2].short(), "Limit Power");
        assert_eq!(plan(HIGH_PERFORMANCE, "High performance").short(), "High perf.");
    }

    #[test]
    fn reads_real_plans() {
        let ps = plans();
        assert!(!ps.is_empty(), "every Windows PC has at least one plan");
        assert!(ps.iter().any(|p| Some(p.guid) == active()));
    }
}
