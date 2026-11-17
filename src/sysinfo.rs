//! System information Dragon Center doesn't show: CPU / RAM usage, NVIDIA
//! GPU stats, power state and battery health. All read with plain Windows
//! APIs (no admin, no WMI/PowerShell), except the GPU which asks
//! `nvidia-smi` (ships with the NVIDIA driver).

use std::ffi::c_void;
use std::os::windows::process::CommandExt;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FileTime {
    lo: u32,
    hi: u32,
}
impl FileTime {
    fn v(self) -> u64 {
        ((self.hi as u64) << 32) | self.lo as u64
    }
}

#[repr(C)]
struct MemoryStatusEx {
    length: u32,
    load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page: u64,
    avail_page: u64,
    total_virt: u64,
    avail_virt: u64,
    avail_ext: u64,
}

#[repr(C)]
#[derive(Default)]
struct SystemPowerStatus {
    ac_line: u8,
    flags: u8,
    percent: u8,
    saver: u8,
    life_secs: u32,
    full_secs: u32,
}

#[repr(C)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

#[repr(C)]
struct SpDeviceInterfaceData {
    size: u32,
    class: Guid,
    flags: u32,
    reserved: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    fn GlobalMemoryStatusEx(s: *mut MemoryStatusEx) -> i32;
    fn GetSystemPowerStatus(s: *mut SystemPowerStatus) -> i32;
    fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *const c_void, disp: u32, flags: u32, tmpl: *mut c_void) -> *mut c_void;
    fn DeviceIoControl(h: *mut c_void, code: u32, inb: *const c_void, insz: u32, outb: *mut c_void, outsz: u32, ret: *mut u32, ov: *mut c_void) -> i32;
    fn CloseHandle(h: *mut c_void) -> i32;
}

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(class: *const Guid, enumerator: *const u16, parent: *mut c_void, flags: u32) -> *mut c_void;
    fn SetupDiEnumDeviceInterfaces(set: *mut c_void, info: *mut c_void, class: *const Guid, index: u32, data: *mut SpDeviceInterfaceData) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(set: *mut c_void, data: *mut SpDeviceInterfaceData, detail: *mut u8, size: u32, required: *mut u32, info: *mut c_void) -> i32;
    fn SetupDiDestroyDeviceInfoList(set: *mut c_void) -> i32;
}

// ---------------------------------------------------------------- CPU / RAM

/// CPU usage from the difference between two `GetSystemTimes` samples.
#[derive(Default)]
pub struct CpuMeter {
    last: Option<(u64, u64)>, // (idle, total)
}

impl CpuMeter {
    /// Percent busy since the previous call (None on the first call).
    pub fn sample(&mut self) -> Option<f32> {
        let (mut i, mut k, mut u) = (FileTime::default(), FileTime::default(), FileTime::default());
        // SAFETY: three valid out-pointers.
        if unsafe { GetSystemTimes(&mut i, &mut k, &mut u) } == 0 {
            return None;
        }
        let idle = i.v();
        let total = k.v() + u.v(); // kernel time includes idle time
        let r = self.last.and_then(|(pi, pt)| {
            let dt = total.saturating_sub(pt);
            (dt > 0).then(|| 100.0 * (1.0 - idle.saturating_sub(pi) as f32 / dt as f32))
        });
        self.last = Some((idle, total));
        r.map(|v| v.clamp(0.0, 100.0))
    }
}

/// (used GiB, total GiB).
pub fn memory() -> Option<(f32, f32)> {
    let mut m = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page: 0,
        avail_page: 0,
        total_virt: 0,
        avail_virt: 0,
        avail_ext: 0,
    };
    // SAFETY: struct with correct length field.
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return None;
    }
    let gib = 1024.0 * 1024.0 * 1024.0;
    Some(((m.total_phys - m.avail_phys) as f32 / gib, m.total_phys as f32 / gib))
}

/// (plugged in, battery percent).
pub fn power() -> Option<(bool, u8)> {
    let mut s = SystemPowerStatus::default();
    // SAFETY: correctly laid out SYSTEM_POWER_STATUS.
    if unsafe { GetSystemPowerStatus(&mut s) } == 0 || s.percent == 255 {
        return None;
    }
    Some((s.ac_line == 1, s.percent))
}

/// True if Windows currently has at least one camera (any device exposing
/// the KSCATEGORY_VIDEO_CAMERA interface; scanners don't).
pub fn camera_present() -> bool {
    const KSCATEGORY_VIDEO_CAMERA: Guid = Guid {
        d1: 0xE532_3777,
        d2: 0xF976,
        d3: 0x4F5B,
        d4: [0x9B, 0x55, 0xB9, 0x46, 0x99, 0xC4, 0x6E, 0x44],
    };
    const DIGCF_PRESENT: u32 = 0x2;
    const DIGCF_DEVICEINTERFACE: u32 = 0x10;
    // SAFETY: standard SetupAPI enumeration; the list is destroyed below.
    unsafe {
        let set = SetupDiGetClassDevsW(&KSCATEGORY_VIDEO_CAMERA, std::ptr::null(), std::ptr::null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set.is_null() || set as isize == -1 {
            return false;
        }
        let mut d = SpDeviceInterfaceData {
            size: std::mem::size_of::<SpDeviceInterfaceData>() as u32,
            class: Guid { d1: 0, d2: 0, d3: 0, d4: [0; 8] },
            flags: 0,
            reserved: 0,
        };
        let found = SetupDiEnumDeviceInterfaces(set, std::ptr::null_mut(), &KSCATEGORY_VIDEO_CAMERA, 0, &mut d) != 0;
        SetupDiDestroyDeviceInfoList(set);
        found
    }
}

// ---------------------------------------------------------------- GPU

#[derive(Debug, Clone, Default)]
pub struct Gpu {
    pub name: String,
    pub util_pct: f32,
    pub clock_mhz: f32,
    pub power_w: Option<f32>,
    pub mem_used_mb: f32,
    pub mem_total_mb: f32,
}

/// Queries the NVIDIA GPU via nvidia-smi (~50 ms). None if unavailable.
pub fn gpu() -> Option<Gpu> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // full path (System32, or NVIDIA's folder under Program Files): the app
    // runs as admin, so never a look-alike found through PATH
    let exe = [Some(crate::elevation::system_dir()), crate::elevation::program_files().map(|p| p.join(r"NVIDIA Corporation\NVSMI"))]
        .into_iter()
        .flatten()
        .map(|d| d.join("nvidia-smi.exe"))
        .find(|p| p.is_file())?;
    let out = std::process::Command::new(exe)
        .args([
            "--query-gpu=name,utilization.gpu,clocks.gr,power.draw,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = text.lines().next()?.split(',').map(str::trim).collect();
    if f.len() < 6 {
        return None;
    }
    let num = |s: &str| s.parse::<f32>().ok();
    Some(Gpu {
        name: f[0].to_string(),
        util_pct: num(f[1]).unwrap_or(0.0),
        clock_mhz: num(f[2]).unwrap_or(0.0),
        power_w: num(f[3]),
        mem_used_mb: num(f[4]).unwrap_or(0.0),
        mem_total_mb: num(f[5]).unwrap_or(0.0),
    })
}

// ---------------------------------------------------------------- battery

#[derive(Debug, Clone, Default)]
pub struct Battery {
    pub chemistry: String,
    pub design_mwh: u32,
    pub full_mwh: u32,
    pub now_mwh: u32,
    pub cycles: u32,
    /// mW, positive = charging, negative = discharging
    pub rate_mw: i32,
    pub voltage_mv: u32,
}

impl Battery {
    /// Full-charge capacity as % of the original design capacity.
    pub fn health_pct(&self) -> Option<f32> {
        (self.design_mwh > 0).then(|| 100.0 * self.full_mwh as f32 / self.design_mwh as f32)
    }
}

const GUID_DEVCLASS_BATTERY: Guid = Guid {
    d1: 0x72631E54,
    d2: 0x78A4,
    d3: 0x11D0,
    d4: [0xBC, 0xF7, 0x00, 0xAA, 0x00, 0xB7, 0xB3, 0x2A],
};
const IOCTL_BATTERY_QUERY_TAG: u32 = 0x0029_4040;
const IOCTL_BATTERY_QUERY_INFORMATION: u32 = 0x0029_4044;
const IOCTL_BATTERY_QUERY_STATUS: u32 = 0x0029_404C;

#[repr(C)]
struct BatteryQueryInformation {
    tag: u32,
    level: u32, // 0 = BatteryInformation
    at_rate: i32,
}

#[repr(C)]
#[derive(Default)]
struct BatteryInformation {
    capabilities: u32,
    technology: u8,
    reserved: [u8; 3],
    chemistry: [u8; 4],
    designed: u32,
    full: u32,
    alert1: u32,
    alert2: u32,
    critical_bias: u32,
    cycles: u32,
}

#[repr(C)]
struct BatteryWaitStatus {
    tag: u32,
    timeout: u32,
    power_state: u32,
    low: u32,
    high: u32,
}

#[repr(C)]
#[derive(Default)]
struct BatteryStatus {
    power_state: u32,
    capacity: u32,
    voltage: u32,
    rate: i32,
}

/// Reads the first battery through the standard battery-class IOCTLs.
pub fn battery() -> Option<Battery> {
    const DIGCF_PRESENT: u32 = 0x2;
    const DIGCF_DEVICEINTERFACE: u32 = 0x10;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const INVALID: isize = -1;

    // SAFETY: all calls below are plain Win32 with buffers we own and sizes we pass.
    unsafe {
        let set = SetupDiGetClassDevsW(&GUID_DEVCLASS_BATTERY, std::ptr::null(), std::ptr::null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set as isize == INVALID {
            return None;
        }
        let mut did = SpDeviceInterfaceData {
            size: std::mem::size_of::<SpDeviceInterfaceData>() as u32,
            class: Guid { d1: 0, d2: 0, d3: 0, d4: [0; 8] },
            flags: 0,
            reserved: 0,
        };
        let mut path: Option<Vec<u16>> = None;
        if SetupDiEnumDeviceInterfaces(set, std::ptr::null_mut(), &GUID_DEVCLASS_BATTERY, 0, &mut did) != 0 {
            let mut need = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &mut did, std::ptr::null_mut(), 0, &mut need, std::ptr::null_mut());
            if need > 4 {
                let mut buf = vec![0u8; need as usize];
                // SP_DEVICE_INTERFACE_DETAIL_DATA_W.cbSize is 8 on x64
                buf[..4].copy_from_slice(&8u32.to_le_bytes());
                if SetupDiGetDeviceInterfaceDetailW(set, &mut did, buf.as_mut_ptr(), need, &mut need, std::ptr::null_mut()) != 0 {
                    let wide: Vec<u16> = buf[4..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
                    let mut p = wide[..end].to_vec();
                    p.push(0);
                    path = Some(p);
                }
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        let path = path?;

        let h = CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, 3, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
        if h as isize == INVALID {
            return None;
        }
        let mut ret = 0u32;
        let wait = 0u32;
        let mut tag = 0u32;
        let ok = DeviceIoControl(h, IOCTL_BATTERY_QUERY_TAG, (&wait as *const u32).cast(), 4, (&mut tag as *mut u32).cast(), 4, &mut ret, std::ptr::null_mut());
        if ok == 0 || tag == 0 {
            CloseHandle(h);
            return None;
        }
        let q = BatteryQueryInformation { tag, level: 0, at_rate: 0 };
        let mut info = BatteryInformation::default();
        DeviceIoControl(
            h,
            IOCTL_BATTERY_QUERY_INFORMATION,
            (&q as *const BatteryQueryInformation).cast(),
            std::mem::size_of::<BatteryQueryInformation>() as u32,
            (&mut info as *mut BatteryInformation).cast(),
            std::mem::size_of::<BatteryInformation>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        );
        let w = BatteryWaitStatus { tag, timeout: 0, power_state: 0, low: 0, high: 0 };
        let mut st = BatteryStatus::default();
        DeviceIoControl(
            h,
            IOCTL_BATTERY_QUERY_STATUS,
            (&w as *const BatteryWaitStatus).cast(),
            std::mem::size_of::<BatteryWaitStatus>() as u32,
            (&mut st as *mut BatteryStatus).cast(),
            std::mem::size_of::<BatteryStatus>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        );
        CloseHandle(h);

        const UNKNOWN: u32 = 0xFFFF_FFFF;
        let clean = |v: u32| if v == UNKNOWN { 0 } else { v };
        Some(Battery {
            chemistry: String::from_utf8_lossy(&info.chemistry).trim_matches(char::from(0)).trim().to_string(),
            design_mwh: clean(info.designed),
            full_mwh: clean(info.full),
            now_mwh: clean(st.capacity),
            cycles: info.cycles,
            rate_mw: if st.rate as u32 == 0x8000_0000 { 0 } else { st.rate },
            voltage_mv: clean(st.voltage),
        })
    }
}

// ------------------------------------------------------- program in front

#[link(name = "user32")]
unsafe extern "system" {
    fn GetForegroundWindow() -> *mut c_void;
    fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn QueryFullProcessImageNameW(process: *mut c_void, flags: u32, name: *mut u16, size: *mut u32) -> i32;
    #[link_name = "CloseHandle"]
    fn CloseProcessHandle(h: *mut c_void) -> i32;
}

/// File name of the program whose window is in front, lower case
/// ("game.exe"). None for the desktop or if it can't be read.
pub fn foreground_app() -> Option<String> {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    let mut pid = 0u32;
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    // SAFETY: plain queries; the buffer is passed with its length; the
    // process handle is closed again.
    let ok = unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() || GetWindowThreadProcessId(hwnd, &mut pid) == 0 || pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseProcessHandle(h);
        ok
    };
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let name = path.rsplit(['\\', '/']).next()?.to_lowercase();
    (!name.is_empty()).then_some(name)
}
