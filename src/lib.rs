//! DragonSlayer: a lightweight replacement for MSI Dragon Center on the
//! MSI GF63 Thin 10UC.

pub mod automation;
#[cfg(windows)]
pub mod autostart;
pub mod controller;
pub mod ec;
#[cfg(windows)]
pub mod elevation;
pub mod icon;
#[cfg(windows)]
pub mod power;
pub mod settings;
pub mod smart;
#[cfg(windows)]
pub mod sysinfo;
#[cfg(windows)]
pub mod winkey;
