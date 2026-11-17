//! Hardware backend through PawnIO (signed driver, https://pawnio.eu).
//!
//! PawnIO runs small signed "modules". We use `LpcACPIEC`, which only allows
//! byte reads/writes on the two ACPI EC ports (0x62 data, 0x66 command/status).
//! The ACPI EC protocol itself is implemented here:
//!
//!   read  addr:  wait IBF=0, 0x80 -> 0x66, wait IBF=0, addr -> 0x62, wait OBF=1, read 0x62
//!   write addr:  wait IBF=0, 0x81 -> 0x66, wait IBF=0, addr -> 0x62, wait IBF=0, value -> 0x62, wait IBF=0
//!
//! Every transaction holds the global `Access_EC` mutex that other EC tools
//! (LibreHardwareMonitor, FanControl, ...) use, and every wait has a timeout.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::backend::{EcBackend, EcError, EcResult};

const EC_DATA: u64 = 0x62;
const EC_CMD: u64 = 0x66;
const OBF: u8 = 0x01; // output buffer full: a byte is waiting for us
const IBF: u8 = 0x02; // input buffer full: EC still busy with our last byte
const RD_EC: u8 = 0x80;
const WR_EC: u8 = 0x81;
const WAIT: Duration = Duration::from_millis(50);
/// Windows' own ACPI driver (and Dragon Center's service, while installed)
/// use the same ports and don't take our lock, so a transaction can get
/// interleaved and lost. Re-running a whole transaction is safe: a read has
/// no side effects and a write stores the same byte again.
const ATTEMPTS: u32 = 6;
const MODULE_FILE: &str = "LpcACPIEC.bin";

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn CreateMutexW(attrs: *const c_void, initial_owner: i32, name: *const u16) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, ms: u32) -> u32;
    fn ReleaseMutex(handle: *mut c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

type OpenFn = unsafe extern "system" fn(handle: *mut *mut c_void) -> i32;
type LoadFn = unsafe extern "system" fn(handle: *mut c_void, blob: *const u8, size: usize) -> i32;
type ExecFn = unsafe extern "system" fn(
    handle: *mut c_void,
    name: *const u8,
    input: *const u64,
    in_size: usize,
    output: *mut u64,
    out_size: usize,
    return_size: *mut usize,
) -> i32;
type CloseFn = unsafe extern "system" fn(handle: *mut c_void) -> i32;

pub struct PawnIoEc {
    handle: *mut c_void,
    exec: ExecFn,
    close: CloseFn,
    mutex: *mut c_void,
}

// Handles are only used by the owning thread (the Controller serializes access).
unsafe impl Send for PawnIoEc {}

fn io(msg: impl Into<String>) -> EcError {
    EcError::Io(msg.into())
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// Where PawnIOLib.dll lives: PawnIO's folder under Program Files, found via
/// the registry. No environment-variable overrides: the app gets admin rights
/// without a prompt, so nothing a normal program can set may pick the DLL.
fn dll_candidates() -> Vec<PathBuf> {
    crate::elevation::program_files().map(|pf| pf.join("PawnIO").join("PawnIOLib.dll")).into_iter().collect()
}

/// Where the signed EC module may live: next to the exe, or in the repo's
/// vendor folder when running a dev build from `target\release`.
fn module_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        v.push(dir.join(MODULE_FILE));
        v.push(dir.join("modules").join(MODULE_FILE));
        if !crate::elevation::is_protected_install(&exe) {
            v.push(dir.join(r"..\..\vendor\pawnio").join(MODULE_FILE));
        }
    }
    v
}

fn hr(code: i32) -> String {
    format!("HRESULT 0x{:08X}", code as u32)
}

impl PawnIoEc {
    pub fn open() -> EcResult<Self> {
        let dll = dll_candidates()
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| io("PawnIO is not installed (PawnIOLib.dll not found)"))?;
        let module_path = module_candidates()
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| io(format!("{MODULE_FILE} not found next to the app")))?;
        let blob = std::fs::read(&module_path).map_err(|e| io(format!("reading {MODULE_FILE}: {e}")))?;

        // SAFETY: NUL-terminated wide path.
        let lib = unsafe { LoadLibraryW(wide(dll.as_os_str()).as_ptr()) };
        if lib.is_null() {
            return Err(io(format!("failed to load {}", dll.display())));
        }
        let sym = |name: &std::ffi::CStr| {
            // SAFETY: lib is a loaded module, name is NUL-terminated.
            let p = unsafe { GetProcAddress(lib, name.as_ptr().cast()) };
            if p.is_null() { Err(io(format!("PawnIOLib.dll has no {name:?}"))) } else { Ok(p) }
        };
        // SAFETY: signatures from PawnIOLib.h (HRESULT STDAPICALLTYPE).
        let open: OpenFn = unsafe { std::mem::transmute(sym(c"pawnio_open")?) };
        let load: LoadFn = unsafe { std::mem::transmute(sym(c"pawnio_load")?) };
        let exec: ExecFn = unsafe { std::mem::transmute(sym(c"pawnio_execute")?) };
        let close: CloseFn = unsafe { std::mem::transmute(sym(c"pawnio_close")?) };

        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: out-pointer to a HANDLE.
        let r = unsafe { open(&mut handle) };
        if r < 0 {
            // E_ACCESSDENIED (0x80070005) -> needs admin
            let hint = if r as u32 == 0x8007_0005 { " - run as Administrator" } else { "" };
            return Err(io(format!("cannot open PawnIO ({}){hint}", hr(r))));
        }
        // SAFETY: valid handle, blob pointer/len from a Vec.
        let r = unsafe { load(handle, blob.as_ptr(), blob.len()) };
        if r < 0 {
            // SAFETY: closing the handle we opened.
            unsafe { close(handle) };
            return Err(io(format!("PawnIO refused the EC module ({})", hr(r))));
        }

        // Shared lock with other EC tools; fall back to no lock if unavailable.
        let name = wide(std::ffi::OsStr::new(r"Global\Access_EC"));
        // SAFETY: NUL-terminated name; null attrs = default security.
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };

        Ok(Self { handle, exec, close, mutex })
    }

    fn call(&self, func: &std::ffi::CStr, input: &[u64], out: &mut [u64]) -> EcResult<()> {
        let mut got = 0usize;
        // SAFETY: buffers are valid for the given cell counts; name is NUL-terminated.
        let r = unsafe {
            (self.exec)(self.handle, func.as_ptr().cast(), input.as_ptr(), input.len(), out.as_mut_ptr(), out.len(), &mut got)
        };
        if r < 0 {
            return Err(io(format!("PawnIO {func:?} failed ({})", hr(r))));
        }
        Ok(())
    }

    fn inb(&self, port: u64) -> EcResult<u8> {
        let mut out = [0u64; 1];
        self.call(c"ioctl_pio_read", &[port], &mut out)?;
        Ok(out[0] as u8)
    }

    fn outb(&self, port: u64, value: u8) -> EcResult<()> {
        self.call(c"ioctl_pio_write", &[port, value as u64], &mut [])
    }

    fn wait(&self, what: &str, ready: impl Fn(u8) -> bool) -> EcResult<()> {
        let start = Instant::now();
        loop {
            if ready(self.inb(EC_CMD)?) {
                return Ok(());
            }
            if start.elapsed() > WAIT {
                return Err(io(format!("EC timeout waiting for {what}")));
            }
            std::hint::spin_loop();
        }
    }

    fn ibf_clear(&self) -> EcResult<()> {
        self.wait("input buffer", |s| s & IBF == 0)
    }

    fn obf_set(&self) -> EcResult<()> {
        self.wait("output byte", |s| s & OBF != 0)
    }

    /// Before starting, make sure no stale byte is sitting in the output
    /// buffer (it would be mistaken for our answer). Give its owner a moment
    /// to collect it, then discard it.
    fn drain(&self) -> EcResult<()> {
        let start = Instant::now();
        loop {
            if self.inb(EC_CMD)? & OBF == 0 {
                return Ok(());
            }
            if start.elapsed() > Duration::from_millis(5) {
                self.inb(EC_DATA)?;
                return Ok(());
            }
            std::hint::spin_loop();
        }
    }

    /// Runs one locked transaction, retrying with short back-off on failure.
    fn transaction<T>(&self, f: impl Fn(&Self) -> EcResult<T>) -> EcResult<T> {
        let mut last = None;
        for attempt in 0..ATTEMPTS {
            match self.locked(|| f(self)) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(Duration::from_millis(1 << attempt)); // 1,2,4,8,16,32 ms
                }
            }
        }
        Err(last.unwrap_or_else(|| io("EC transaction failed")))
    }

    /// Runs `f` while holding the shared EC lock (if we have it).
    fn locked<T>(&self, f: impl FnOnce() -> EcResult<T>) -> EcResult<T> {
        let mut held = false;
        if !self.mutex.is_null() {
            // SAFETY: valid mutex handle. 0 = acquired, 0x80 = abandoned (still ours).
            let w = unsafe { WaitForSingleObject(self.mutex, 200) };
            if w == 0 || w == 0x80 {
                held = true;
            } else {
                return Err(io("EC is busy (another tool holds Access_EC)"));
            }
        }
        let r = f();
        if held {
            // SAFETY: we own the mutex.
            unsafe { ReleaseMutex(self.mutex) };
        }
        r
    }
}

impl Drop for PawnIoEc {
    fn drop(&mut self) {
        // SAFETY: closing handles we opened.
        unsafe {
            (self.close)(self.handle);
            if !self.mutex.is_null() {
                CloseHandle(self.mutex);
            }
        }
    }
}

impl EcBackend for PawnIoEc {
    fn read(&mut self, addr: u8) -> EcResult<u8> {
        self.transaction(|s| {
            s.ibf_clear()?;
            s.drain()?;
            s.outb(EC_CMD, RD_EC)?;
            s.ibf_clear()?;
            s.outb(EC_DATA, addr)?;
            s.obf_set()?;
            s.inb(EC_DATA)
        })
    }

    fn write(&mut self, addr: u8, value: u8) -> EcResult<()> {
        self.transaction(|s| {
            s.ibf_clear()?;
            s.drain()?;
            s.outb(EC_CMD, WR_EC)?;
            s.ibf_clear()?;
            s.outb(EC_DATA, addr)?;
            s.ibf_clear()?;
            s.outb(EC_DATA, value)?;
            s.ibf_clear()
        })
    }
}
