//! Real hardware backend: the same route MSI's own service uses.
//!
//! MSI ships the WinIO 3.x kernel driver renamed to `KernCoreLib64.sys`
//! (service `WINIO`, device `\\.\WINIO`) plus its helper DLL
//! `KernCoreLib64.dll`. The EC's 256 registers are memory-mapped at physical
//! address 0xFC000800 (MSIAPService_Log: "ReadMemory Address :FC0008EF").
//!
//! Reads:  through a mapping of the EC page (or `GetPhysLong` as fallback).
//! Writes: exactly ONE byte through the mapping. The driver's `SetPhysLong`
//!         writes 4 bytes and would clobber the 3 neighbouring registers, so
//!         it is never used.
//!
//! Rules this backend follows:
//!   * Only attaches if MSI's driver is ALREADY running. It never installs,
//!     starts, stops or removes the driver service.
//!   * Never calls `ShutdownKernCoreLib` (stock WinIO removes the driver
//!     service on shutdown, which would break MSI's service).
//!   * Writes are enabled only if the mapping passes a self-check: the
//!     static firmware bytes read through the mapping must equal what the
//!     driver's own `GetPhysLong` returns.

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use super::backend::{EcBackend, EcError, EcResult};
use super::map;

const DLL_PATH: &str = r"C:\Program Files (x86)\MSI\MSI NBFoundation Service\KernCoreLib64.dll";
const DEVICE: &str = r"\\.\WINIO";

/// Page that contains the EC window (64 KiB aligned, so no rounding issues).
const PAGE_BASE: u64 = 0xFC00_0000;
const PAGE_SIZE: u64 = 0x1000;
/// The EC's 256 registers start here inside that page.
const EC_OFFSET: usize = 0x800;
const EC_RAM_BASE: usize = PAGE_BASE as usize + EC_OFFSET;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

/// WinIO 3.x `tagPhysStruct`.
#[repr(C)]
#[derive(Debug, Default)]
struct PhysStruct {
    size: u64,
    phys_addr: u64,
    handle: u64,
    lin: u64,
    section: u64,
}

/// `bool InitializeWinIo()`
type InitFn = unsafe extern "system" fn() -> u8;
/// `bool GetPhysLong(PBYTE physAddr, PDWORD value)` — reads 4 bytes.
type GetPhysLongFn = unsafe extern "system" fn(addr: *mut u8, value: *mut u32) -> u8;
/// `PBYTE MapPhysToLin(tagPhysStruct &)`
type MapFn = unsafe extern "system" fn(ps: *mut PhysStruct) -> *mut u8;
/// `bool UnmapPhysicalMemory(tagPhysStruct &)`
type UnmapFn = unsafe extern "system" fn(ps: *mut PhysStruct) -> u8;

struct Mapping {
    ptr: *mut u8,
    ps: PhysStruct,
    unmap: UnmapFn,
}

pub struct WinIoEc {
    get_phys_long: GetPhysLongFn,
    mapping: Option<Mapping>,
    /// Why writes are disabled, if they are.
    write_block: Option<String>,
}

// The raw mapping pointer is only ever used by the thread that owns the
// backend (the Controller serializes all access), so moving it is fine.
unsafe impl Send for WinIoEc {}

fn io(msg: impl Into<String>) -> EcError {
    EcError::Io(msg.into())
}

/// Looks up `name` in `module`; null if missing.
fn proc(module: *mut c_void, name: &std::ffi::CStr) -> *mut c_void {
    // SAFETY: module is a loaded DLL handle and name is NUL-terminated.
    unsafe { GetProcAddress(module, name.as_ptr().cast()) }
}

impl WinIoEc {
    pub fn open() -> EcResult<Self> {
        // 1. Only proceed if MSI's driver is already loaded. (If the DLL found
        //    it missing it would try to re-install the service — never wanted.)
        match OpenOptions::new().read(true).write(true).open(DEVICE) {
            Ok(_probe) => {}
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                return Err(io("access denied opening MSI's driver - run as Administrator"));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(io(
                    "MSI's WINIO driver is not running (is MSI Foundation Service started?)",
                ));
            }
            Err(e) => return Err(io(format!("cannot open {DEVICE}: {e}"))),
        }

        // 2. Load MSI's helper DLL by full path.
        if !Path::new(DLL_PATH).is_file() {
            return Err(io(format!("{DLL_PATH} not found")));
        }
        let wide: Vec<u16> = Path::new(DLL_PATH).as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 path.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            return Err(io("failed to load KernCoreLib64.dll"));
        }

        let init = proc(module, c"InitializeKernCoreLib");
        let get = proc(module, c"GetPhysLong");
        if init.is_null() || get.is_null() {
            return Err(io("KernCoreLib64.dll is missing expected functions"));
        }
        // SAFETY: stock WinIO 3.x exports with these signatures. (x64 WinIO
        // only exists from 3.0 on, and the DLL's PDB path says Projects\WinIo.)
        let init: InitFn = unsafe { std::mem::transmute(init) };
        let get_phys_long: GetPhysLongFn = unsafe { std::mem::transmute(get) };

        // SAFETY: the driver device exists (checked above); this only opens it.
        if unsafe { init() } == 0 {
            return Err(io("KernCoreLib initialisation failed"));
        }

        let mut ec = Self { get_phys_long, mapping: None, write_block: None };
        ec.try_map(module);
        Ok(ec)
    }

    fn read_via_driver(&self, addr: u8) -> EcResult<u8> {
        let mut value: u32 = 0;
        let phys = (EC_RAM_BASE + addr as usize) as *mut u8;
        // SAFETY: `phys` is a number (physical address) the driver maps; it is
        // never dereferenced in our process.
        let ok = unsafe { (self.get_phys_long)(phys, &mut value) };
        if ok == 0 {
            return Err(io(format!("driver failed to read EC address 0x{addr:02X}")));
        }
        Ok(value as u8) // little-endian: low byte is the byte at `phys`
    }

    /// Maps the EC page and enables writes if the self-check passes.
    fn try_map(&mut self, module: *mut c_void) {
        let map_p = proc(module, c"MapPhysToLin");
        let unmap_p = proc(module, c"UnmapPhysicalMemory");
        if map_p.is_null() || unmap_p.is_null() {
            self.write_block = Some("driver DLL has no memory-mapping functions".into());
            return;
        }
        // SAFETY: WinIO 3.x signatures (see module docs).
        let map_fn: MapFn = unsafe { std::mem::transmute(map_p) };
        let unmap: UnmapFn = unsafe { std::mem::transmute(unmap_p) };

        let mut ps = PhysStruct { size: PAGE_SIZE, phys_addr: PAGE_BASE, ..Default::default() };
        // SAFETY: `ps` is a valid, initialised tagPhysStruct.
        let ptr = unsafe { map_fn(&mut ps) };
        if ptr.is_null() {
            self.write_block = Some("could not map the EC memory page".into());
            return;
        }
        let mut mapping = Mapping { ptr, ps, unmap };

        // Self-check: the firmware string area never changes, so the mapping
        // and the driver must agree on every byte of it, and it must be text.
        let start = map::FW_VERSION_ADDR;
        let mut ok = true;
        for a in start..start + 28 {
            // SAFETY: EC_OFFSET + a < PAGE_SIZE, inside the mapped page.
            let via_map = unsafe { std::ptr::read_volatile(ptr.add(EC_OFFSET + a as usize)) };
            match self.read_via_driver(a) {
                Ok(v) if v == via_map => {}
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        // SAFETY: as above.
        let first = unsafe { std::ptr::read_volatile(ptr.add(EC_OFFSET + start as usize)) };
        if ok && first.is_ascii_alphanumeric() {
            self.mapping = Some(mapping);
        } else {
            // SAFETY: undoing the mapping we just made.
            unsafe { (mapping.unmap)(&mut mapping.ps) };
            self.write_block = Some("EC page mapping failed its self-check".into());
        }
    }
}

impl Drop for WinIoEc {
    fn drop(&mut self) {
        if let Some(m) = self.mapping.as_mut() {
            // SAFETY: unmapping exactly what MapPhysToLin gave us.
            unsafe { (m.unmap)(&mut m.ps) };
        }
    }
}

impl EcBackend for WinIoEc {
    fn read(&mut self, addr: u8) -> EcResult<u8> {
        match &self.mapping {
            // SAFETY: addr <= 0xFF, so EC_OFFSET + addr < PAGE_SIZE.
            Some(m) => Ok(unsafe { std::ptr::read_volatile(m.ptr.add(EC_OFFSET + addr as usize)) }),
            None => self.read_via_driver(addr),
        }
    }

    fn write(&mut self, addr: u8, value: u8) -> EcResult<()> {
        match &self.mapping {
            Some(m) => {
                // SAFETY: single-byte volatile store inside the mapped EC page.
                unsafe { std::ptr::write_volatile(m.ptr.add(EC_OFFSET + addr as usize), value) };
                Ok(())
            }
            None => Err(io(format!(
                "writes disabled: {}",
                self.write_block.as_deref().unwrap_or("no mapping")
            ))),
        }
    }

    fn can_write(&self) -> bool {
        self.mapping.is_some()
    }
}
