//! Admin rights without a UAC prompt on every click, and one app at a time.
//!
//! PawnIO only admits administrators. The exe itself asks for nothing
//! (asInvoker); a plain click starts a non-admin copy that either wakes the
//! copy already running, or runs the "DragonSlayer Open" task (run with
//! highest privileges, no prompt) and exits. That task only exists for a copy
//! installed under Program Files, where ordinary programs can't swap the exe.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(token: *mut c_void, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn RegGetValueW(key: isize, sub: *const u16, value: *const u16, flags: u32, ty: *mut u32, data: *mut c_void, len: *mut u32) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl: *const u16, rev: u32, sd: *mut *mut c_void, size: *mut u32) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn CreateEventW(sa: *const SecurityAttributes, manual: i32, initial: i32, name: *const u16) -> *mut c_void;
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn SetEvent(h: *mut c_void) -> i32;
    fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
    fn GetSystemDirectoryW(buf: *mut u16, len: u32) -> u32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn AllowSetForegroundWindow(pid: u32) -> i32;
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(hwnd: *mut c_void, verb: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
}

#[repr(C)]
struct SecurityAttributes {
    len: u32,
    sd: *mut c_void,
    inherit: i32,
}

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Is this process running with admin rights (elevated)?
pub fn is_elevated() -> bool {
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION: u32 = 20;
    let mut token = std::ptr::null_mut();
    let mut elevated = 0u32;
    let mut len = 0u32;
    // SAFETY: querying our own process token into a 4-byte TOKEN_ELEVATION.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let ok = GetTokenInformation(token, TOKEN_ELEVATION, (&mut elevated as *mut u32).cast(), 4, &mut len);
        CloseHandle(token);
        ok != 0 && elevated != 0
    }
}

/// "C:\Program Files", read from HKLM (only admins can change it), not from
/// %ProgramFiles%, which any program can override in the user's environment.
pub fn program_files() -> Option<PathBuf> {
    const HKLM: isize = 0x8000_0002_u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x2;
    let sub = wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion");
    let val = wide("ProgramFilesDir");
    let mut buf = [0u16; 260];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: buffer and its byte length passed together.
    let rc = unsafe { RegGetValueW(HKLM, sub.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    if rc != 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// C:\Windows\System32, from the OS (not from PATH or the environment).
pub fn system_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    // SAFETY: buffer and its length in characters.
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return PathBuf::from(r"C:\Windows\System32");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..n]))
}

/// Installed under Program Files, i.e. somewhere only admins can write.
pub fn is_protected_install(exe: &Path) -> bool {
    let Some(pf) = program_files() else { return false };
    let (e, p) = (exe.to_string_lossy().to_lowercase(), pf.to_string_lossy().to_lowercase());
    e.starts_with(&format!("{}\\", p.trim_end_matches('\\')))
}

/// Starts this exe again with admin rights (one UAC prompt).
pub fn relaunch_as_admin(args: &[String]) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let params: String = args.iter().map(|a| format!("\"{}\" ", a.replace('"', ""))).collect();
    let (verb, file, params) = (wide("runas"), wide(exe.as_os_str()), wide(params.trim_end()));
    // SAFETY: NUL-terminated wide strings; SW_SHOWNORMAL.
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), 1) };
    r > 32
}

/// Lets the copy we hand over to (task or running copy) bring its window to
/// the front; Windows only allows that to the process the user clicked.
pub fn allow_foreground() {
    const ASFW_ANY: u32 = u32::MAX;
    // SAFETY: plain call, no pointers.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

// ---- one copy at a time -----------------------------------------------------

/// Auto-reset event the running (admin) copy waits on; a second launch sets
/// it to bring the window forward. Its existence = "already running".
const SHOW_EVENT: &str = r"Local\DragonSlayer.Show";

/// If a copy is already running, ask it to show its window. True if one was.
pub fn signal_existing() -> bool {
    const EVENT_MODIFY_STATE: u32 = 0x0002;
    let name = wide(SHOW_EVENT);
    // SAFETY: plain named-event open/set/close.
    unsafe {
        let h = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if h.is_null() {
            return false;
        }
        SetEvent(h);
        CloseHandle(h);
    }
    true
}

/// Waits (up to `timeout`) for a copy to start, e.g. after running the task.
pub fn wait_for_running(timeout: Duration) -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    let name = wide(SHOW_EVENT);
    let start = Instant::now();
    while start.elapsed() < timeout {
        // SAFETY: plain named-event open/close.
        let h = unsafe { OpenEventW(SYNCHRONIZE, 0, name.as_ptr()) };
        if !h.is_null() {
            unsafe { CloseHandle(h) };
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// In the running copy: calls `on_show` whenever another launch asks for the
/// window. The event lets the user's normal (non-admin) processes signal it:
/// interactive users may set it, and its integrity label is lowered to medium.
pub fn listen_for_show(on_show: impl Fn() + Send + 'static) -> bool {
    let sddl = wide("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100002;;;IU)S:(ML;;NW;;;ME)");
    let mut sd = std::ptr::null_mut();
    // SAFETY: SDDL string -> self-relative security descriptor (kept for the
    // process lifetime, like the event).
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut()) } == 0 {
        return false;
    }
    let sa = SecurityAttributes { len: size_of::<SecurityAttributes>() as u32, sd, inherit: 0 };
    let name = wide(SHOW_EVENT);
    // SAFETY: auto-reset, initially unsignalled named event.
    let h = unsafe { CreateEventW(&sa, 0, 0, name.as_ptr()) };
    if h.is_null() {
        return false;
    }
    let h = h as usize; // raw handle to the waiting thread
    std::thread::spawn(move || {
        loop {
            // SAFETY: valid event handle, never closed.
            if unsafe { WaitForSingleObject(h as *mut c_void, u32::MAX) } != 0 {
                return;
            }
            on_show();
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_files_comes_from_the_registry() {
        let pf = program_files().expect("ProgramFilesDir");
        assert!(pf.is_absolute() && pf.join("..").exists());
        assert!(is_protected_install(&pf.join("DragonSlayer").join("dragonslayer.exe")));
        assert!(!is_protected_install(Path::new(r"D:\MY FILES\PROJECTS\target\release\dragonslayer.exe")));
        // a look-alike folder next to it is not inside it
        let fake = PathBuf::from(format!("{} Evil", pf.display())).join("dragonslayer.exe");
        assert!(!is_protected_install(&fake));
    }
}
