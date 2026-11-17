pub mod backend;
pub mod map;
#[cfg(windows)]
pub mod pawnio;
#[cfg(windows)]
pub mod winio;

#[cfg(windows)]
pub use auto::{AnyEc, open_hardware};

#[cfg(windows)]
mod auto {
    use super::backend::{EcBackend, EcError, EcResult};
    use super::pawnio::PawnIoEc;
    use super::winio::WinIoEc;

    /// Whichever real backend could be opened.
    pub enum AnyEc {
        PawnIo(PawnIoEc),
        WinIo(WinIoEc),
    }

    impl AnyEc {
        pub fn name(&self) -> &'static str {
            match self {
                Self::PawnIo(_) => "PawnIO",
                Self::WinIo(_) => "MSI driver (WinIO)",
            }
        }
    }

    impl EcBackend for AnyEc {
        fn read(&mut self, addr: u8) -> EcResult<u8> {
            match self {
                Self::PawnIo(e) => e.read(addr),
                Self::WinIo(e) => e.read(addr),
            }
        }
        fn write(&mut self, addr: u8, value: u8) -> EcResult<()> {
            match self {
                Self::PawnIo(e) => e.write(addr, value),
                Self::WinIo(e) => e.write(addr, value),
            }
        }
        fn can_write(&self) -> bool {
            match self {
                Self::PawnIo(e) => e.can_write(),
                Self::WinIo(e) => e.can_write(),
            }
        }
    }

    /// `which`: "pawnio", "winio" or "auto" (PawnIO first, MSI driver as a
    /// fallback while Dragon Center is still installed).
    pub fn open_hardware(which: &str) -> EcResult<AnyEc> {
        match which {
            "pawnio" => PawnIoEc::open().map(AnyEc::PawnIo),
            "winio" => WinIoEc::open().map(AnyEc::WinIo),
            _ => match PawnIoEc::open() {
                Ok(e) => Ok(AnyEc::PawnIo(e)),
                Err(pawn_err) => WinIoEc::open().map(AnyEc::WinIo).map_err(|win_err| {
                    EcError::Io(format!("PawnIO: {pawn_err}; MSI driver: {win_err}"))
                }),
            },
        }
    }
}
