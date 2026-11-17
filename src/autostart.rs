//! Task Scheduler tasks that start the app with admin rights, no UAC prompt:
//!
//! - "DragonSlayer": Start with Windows (logon trigger, starts hidden).
//! - "DragonSlayer Open": no trigger; a plain click on the app runs it
//!   (src/elevation.rs). Only registered for a copy under Program Files.
//!
//! Created from XML so they also run on battery and are never time-limited
//! (plain `schtasks /Create` can't set that).

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const TASK: &str = "DragonSlayer";
const OPEN_TASK: &str = "DragonSlayer Open";
/// The logon task's name before the app was renamed.
const OLD_TASK: &str = "MSI Control";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn schtasks(args: &[&str]) -> std::io::Result<std::process::Output> {
    // full path: never a look-alike from PATH in an admin process
    Command::new(crate::elevation::system_dir().join("schtasks.exe")).args(args).creation_flags(CREATE_NO_WINDOW).output()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn exists(name: &str) -> bool {
    schtasks(&["/Query", "/TN", name]).map(|o| o.status.success()).unwrap_or(false)
}

fn delete(name: &str) -> Result<(), String> {
    let out = schtasks(&["/Delete", "/TN", name, "/F"]).map_err(|e| format!("running schtasks: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

pub fn is_enabled() -> bool {
    exists(TASK)
}

/// Run at start: if Start with Windows is on (under the old name too),
/// re-create the task for THIS exe, so a rename or moved exe can't leave it
/// pointing at a file that's gone. Returns whether it's on.
pub fn refresh(exe: &Path) -> bool {
    let old = exists(OLD_TASK);
    if old {
        let _ = delete(OLD_TASK);
    }
    if old || is_enabled() { enable(exe).is_ok() } else { false }
}

pub fn enable(exe: &Path) -> Result<(), String> {
    create(TASK, exe, true)
}

pub fn disable() -> Result<(), String> {
    delete(TASK)
}

/// Keeps "DragonSlayer Open" pointing at this exe, or removes it when this
/// copy isn't installed under Program Files (a no-prompt admin launch of an
/// exe that any program could overwrite would be a way around UAC).
pub fn sync_open_task(exe: &Path) {
    if crate::elevation::is_protected_install(exe) {
        let _ = create(OPEN_TASK, exe, false);
    } else if exists(OPEN_TASK) {
        let _ = delete(OPEN_TASK);
    }
}

/// From a plain (non-admin) launch: start the admin copy via the task.
pub fn run_open_task() -> bool {
    schtasks(&["/Run", "/TN", OPEN_TASK]).map(|o| o.status.success()).unwrap_or(false)
}

fn create(name: &str, exe: &Path, at_logon: bool) -> Result<(), String> {
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let user = xml_escape(&user);
    let (description, triggers, args) = if at_logon {
        (
            "Starts DragonSlayer at login and restores fan, battery and keyboard settings.",
            format!("<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId><Delay>PT5S</Delay></LogonTrigger></Triggers>"),
            "<Arguments>--startup</Arguments>",
        )
    } else {
        ("Opens DragonSlayer with admin rights when you click its icon (no UAC prompt).", String::new(), "")
    };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{description}</Description>
  </RegistrationInfo>
  {triggers}
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      {args}
    </Exec>
  </Actions>
</Task>
"#,
        exe = xml_escape(&exe.display().to_string()),
    );

    // Task Scheduler wants UTF-16 with a BOM.
    let mut bytes = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let path = std::env::temp_dir().join(format!("dragonslayer-task-{}.xml", std::process::id()));
    std::fs::write(&path, bytes).map_err(|e| format!("writing task file: {e}"))?;
    let out = schtasks(&["/Create", "/TN", name, "/XML", &path.display().to_string(), "/F"])
        .map_err(|e| format!("running schtasks: {e}"))?;
    let _ = std::fs::remove_file(&path);
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}
