//! Windows-key lock: a low-level keyboard hook that swallows the Windows
//! keys while locked, so a stray press can't throw you out of a game.
//! Pure software — nothing is written to the laptop.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static LOCKED: AtomicBool = AtomicBool::new(false);
/// Whether we let the key-down of LWin / RWin through (its key-up must follow).
static PASSED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

#[repr(C)]
struct KbdLlHook {
    vk: u32,
    scan: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

type HookProc = unsafe extern "system" fn(code: i32, w: usize, l: isize) -> isize;

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowsHookExW(id: i32, proc: HookProc, module: *mut c_void, thread: u32) -> *mut c_void;
    fn CallNextHookEx(hook: *mut c_void, code: i32, w: usize, l: isize) -> isize;
    fn GetMessageW(msg: *mut [usize; 8], hwnd: *mut c_void, min: u32, max: u32) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

const WH_KEYBOARD_LL: i32 = 13;
const LLKHF_UP: u32 = 0x80;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

unsafe extern "system" fn hook(code: i32, w: usize, l: isize) -> isize {
    if code == 0 {
        // SAFETY: for WH_KEYBOARD_LL with code 0 (HC_ACTION), lParam points
        // to a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(l as *const KbdLlHook) };
        let idx = match k.vk {
            VK_LWIN => Some(0),
            VK_RWIN => Some(1),
            _ => None,
        };
        if let Some(i) = idx {
            let locked = LOCKED.load(Ordering::Relaxed);
            if k.flags & LLKHF_UP == 0 {
                if locked {
                    return 1;
                }
                PASSED[i].store(true, Ordering::Relaxed);
            } else if !PASSED[i].swap(false, Ordering::Relaxed) && locked {
                // a key-up whose key-down we swallowed: swallow it too
                return 1;
            }
        }
    }
    // SAFETY: forwarding the event unchanged.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}

/// Installs the hook on its own thread (once). Returns false if Windows
/// refused it.
pub fn start() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: standard hook install + message loop on this thread.
            unsafe {
                let h = SetWindowsHookExW(WH_KEYBOARD_LL, hook, GetModuleHandleW(std::ptr::null()), 0);
                let _ = tx.send(!h.is_null());
                if h.is_null() {
                    return;
                }
                let mut msg = [0usize; 8];
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
            }
        });
        rx.recv().unwrap_or(false)
    })
}

pub fn set_locked(on: bool) {
    LOCKED.store(on, Ordering::Relaxed);
}

pub fn is_locked() -> bool {
    LOCKED.load(Ordering::Relaxed)
}
