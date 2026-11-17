//! Checks the Windows-key lock without a person at the keyboard.
//! Installs an observer hook FIRST (so the lock's hook, installed second,
//! sees keys before it), locks, then injects F24 and a Windows-key press.
//! Pass: the observer sees F24 (other keys still work) but not the Windows key.
//! If the lock failed the Start menu would open, so Esc is sent to close it.
//! Run: cargo run --release --example winkey_check

use dragonslayer::winkey;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static SAW_F24: AtomicBool = AtomicBool::new(false);
static SAW_WIN: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct KbdLlHook {
    vk: u32,
    scan: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
struct Input {
    kind: u32,
    _pad: u32,
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
    _rest: [u8; 8],
}

type HookProc = unsafe extern "system" fn(i32, usize, isize) -> isize;

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowsHookExW(id: i32, proc: HookProc, module: *mut c_void, thread: u32) -> *mut c_void;
    fn UnhookWindowsHookEx(h: *mut c_void) -> i32;
    fn CallNextHookEx(hook: *mut c_void, code: i32, w: usize, l: isize) -> isize;
    fn GetMessageW(msg: *mut [usize; 8], hwnd: *mut c_void, min: u32, max: u32) -> i32;
    fn PostThreadMessageW(thread: u32, msg: u32, w: usize, l: isize) -> i32;
    fn SendInput(n: u32, inputs: *const Input, size: i32) -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetCurrentThreadId() -> u32;
}

const VK_F24: u16 = 0x87;
const VK_LWIN: u16 = 0x5B;
const VK_ESCAPE: u16 = 0x1B;

unsafe extern "system" fn observer(code: i32, w: usize, l: isize) -> isize {
    if code == 0 {
        // SAFETY: HC_ACTION on WH_KEYBOARD_LL -> KBDLLHOOKSTRUCT.
        let k = unsafe { &*(l as *const KbdLlHook) };
        match k.vk as u16 {
            VK_F24 => SAW_F24.store(true, Ordering::Relaxed),
            VK_LWIN => SAW_WIN.store(true, Ordering::Relaxed),
            _ => {}
        }
    }
    // SAFETY: forwarding unchanged.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}

fn press(vk: u16) {
    let key = |up: bool| Input { kind: 1, _pad: 0, vk, scan: 0, flags: if up { 2 } else { 0 }, time: 0, extra: 0, _rest: [0; 8] };
    let inputs = [key(false), key(true)];
    // SAFETY: two valid INPUT_KEYBOARD records.
    unsafe { SendInput(2, inputs.as_ptr(), size_of::<Input>() as i32) };
}

fn main() {
    assert_eq!(size_of::<Input>(), 40);
    // SAFETY: standard hook install on this thread; the message loop below serves it.
    let h = unsafe { SetWindowsHookExW(13, observer, GetModuleHandleW(std::ptr::null()), 0) };
    assert!(!h.is_null(), "observer hook refused");
    assert!(winkey::start(), "lock hook refused");
    winkey::set_locked(true);

    let main_thread = unsafe { GetCurrentThreadId() };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        press(VK_F24);
        press(VK_LWIN);
        std::thread::sleep(Duration::from_millis(500));
        // SAFETY: WM_QUIT to our own main thread.
        unsafe { PostThreadMessageW(main_thread, 0x0012, 0, 0) };
    });
    let mut msg = [0usize; 8];
    // SAFETY: message loop for the observer hook.
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {}
    unsafe { UnhookWindowsHookEx(h) };

    let f24 = SAW_F24.load(Ordering::Relaxed);
    let win = SAW_WIN.load(Ordering::Relaxed);
    if win {
        std::thread::sleep(Duration::from_millis(300));
        press(VK_ESCAPE); // close the Start menu the leak opened
    }
    println!("other keys pass: {}", if f24 { "yes" } else { "NO" });
    println!("Windows key blocked: {}", if win { "NO" } else { "yes" });
    println!("{}", if f24 && !win { "PASS" } else { "FAIL" });
    std::process::exit(if f24 && !win { 0 } else { 1 });
}
