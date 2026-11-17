//! How bytes get to and from the Embedded Controller.
//!
//! `EcBackend` is the only thing that touches hardware. Today there is a
//! `MockEc` (an in-memory fake chip) for development and tests. The real
//! backend (PawnIO's EC module) plugs in behind the same trait later.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EcError {
    /// The backend failed to talk to the EC (driver missing, timeout, ...).
    Io(String),
    /// The address is not on the write allowlist.
    NotWritable(u8),
    /// The value is not a known-valid value for that address.
    InvalidValue { addr: u8, value: u8 },
    /// EC firmware isn't one this register map was verified against.
    UnsupportedFirmware(String),
    /// We wrote a value but reading it back gave something else.
    VerifyFailed { addr: u8, wrote: u8, read: u8 },
}

impl fmt::Display for EcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "EC I/O error: {e}"),
            Self::NotWritable(a) => write!(f, "EC address 0x{a:02X} is not writable"),
            Self::InvalidValue { addr, value } => {
                write!(f, "0x{value:02X} is not a valid value for EC address 0x{addr:02X}")
            }
            Self::UnsupportedFirmware(fw) => write!(
                f,
                "EC firmware '{fw}' is not a verified model; refusing to write"
            ),
            Self::VerifyFailed { addr, wrote, read } => write!(
                f,
                "wrote 0x{wrote:02X} to 0x{addr:02X} but read back 0x{read:02X}"
            ),
        }
    }
}

impl std::error::Error for EcError {}

pub type EcResult<T> = Result<T, EcError>;

/// Raw byte access to the EC's 256-byte register space.
/// Implementations must serialize access (the EC is not thread-safe).
pub trait EcBackend {
    fn read(&mut self, addr: u8) -> EcResult<u8>;
    fn write(&mut self, addr: u8, value: u8) -> EcResult<()>;

    /// False if this backend can only read.
    fn can_write(&self) -> bool {
        true
    }

    fn read_range(&mut self, start: u8, len: usize) -> EcResult<Vec<u8>> {
        (0..len)
            .map(|i| {
                let addr = start
                    .checked_add(i as u8)
                    .filter(|_| i <= u8::MAX as usize)
                    .ok_or_else(|| EcError::Io("read past end of EC space".into()))?;
                self.read(addr)
            })
            .collect()
    }
}

/// A fake EC held in memory. Starts out looking like a GF63 Thin 10UC
/// idling on Comfort mode, so the app can be developed without hardware.
#[derive(Debug, Clone)]
pub struct MockEc {
    pub regs: [u8; 256],
    /// Every write, in order — lets tests prove what was (not) written.
    pub write_log: Vec<(u8, u8)>,
}

impl MockEc {
    pub fn gf63_idle() -> Self {
        use super::map::*;
        let mut regs = [0u8; 256];
        for (i, b) in b"16R5EMS1.104".iter().enumerate() {
            regs[FW_VERSION_ADDR as usize + i] = *b;
        }
        regs[CPU_TEMP as usize] = 52;
        regs[CPU_FAN_SPEED as usize] = 38;
        regs[GPU_TEMP as usize] = 47;
        regs[GPU_FAN_SPEED as usize] = 35;
        regs[FAN_TACH as usize] = 0x00; // 0x00C8 = 200 -> 2,400 RPM
        regs[FAN_TACH as usize + 1] = 0xC8;
        regs[COOLER_BOOST as usize] = 0x02; // boost off, unrelated bit set
        regs[SHIFT_MODE as usize] = ShiftMode::Comfort.to_byte();
        regs[FAN_MODE as usize] = FanMode::Auto.to_byte();
        regs[CHARGE_LIMIT as usize] = ChargeLimit::new(100).unwrap().to_byte();
        regs[KBD_BACKLIGHT as usize] = KbdBacklight::new(2).unwrap().to_byte();
        regs[WEBCAM as usize] = 0x02; // webcam on
        regs[FN_WIN_SWAP as usize] = 1 << FN_WIN_SWAP_BIT; // inverted bit set = not swapped
        // fan curve tables as read from the real laptop
        let cpu_temps = [55, 64, 73, 76, 82, 88];
        let gpu_temps = [55, 61, 67, 73, 79, 85];
        regs[CPU_CURVE_TEMPS as usize..][..CURVE_STEPS].copy_from_slice(&cpu_temps);
        regs[GPU_CURVE_TEMPS as usize..][..CURVE_STEPS].copy_from_slice(&gpu_temps);
        regs[CPU_CURVE_SPEEDS as usize..][..CURVE_POINTS].copy_from_slice(&FanCurve::FACTORY.speeds());
        regs[GPU_CURVE_SPEEDS as usize..][..CURVE_POINTS].copy_from_slice(&FanCurve::FACTORY.speeds());
        Self { regs, write_log: Vec::new() }
    }

    pub fn with_firmware(mut self, fw: &str) -> Self {
        let start = super::map::FW_VERSION_ADDR as usize;
        let len = super::map::FW_VERSION_LEN;
        self.regs[start..start + len].fill(0);
        for (i, b) in fw.bytes().take(len).enumerate() {
            self.regs[start + i] = b;
        }
        self
    }
}

impl EcBackend for MockEc {
    fn read(&mut self, addr: u8) -> EcResult<u8> {
        Ok(self.regs[addr as usize])
    }

    fn write(&mut self, addr: u8, value: u8) -> EcResult<()> {
        self.regs[addr as usize] = value;
        self.write_log.push((addr, value));
        Ok(())
    }
}
