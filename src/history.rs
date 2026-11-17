//! History log: one line every few seconds with temperatures, load, fan
//! and mode, so fan modes can be compared and Smart mode tuned from what
//! really happened on this laptop instead of from a simulation.
//!
//! One CSV file a day in `%APPDATA%\DragonSlayer\history\`, about 1 MB a
//! day; the newest `KEEP_DAYS` files are kept. Nothing leaves the PC, and
//! no program names are written.

use std::io::Write;
use std::path::PathBuf;

pub const KEEP_DAYS: usize = 14;
pub const EVERY_SECS: u32 = 5;
pub const HEADER: &str = "time,mode,cpu_c,gpu_c,cpu_load,gpu_load,fan_target_pct,rpm,battery,boost,smart_pct,smart_load_level,smart_heat_c,smart_target_c";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    /// auto | silent | custom | smart
    pub mode: &'static str,
    pub cpu_c: u8,
    pub gpu_c: u8,
    pub cpu_load: f32,
    pub gpu_load: f32,
    /// what the curve asks of the fan (the higher of the CPU and GPU sides)
    pub fan_target_pct: u8,
    pub rpm: u16,
    pub on_battery: bool,
    pub boost: bool,
    /// Smart mode's own decision, when it is on: speed, load level, the
    /// smoothed temperature it steers by, and the temperature it aims under
    pub smart: Option<(u8, usize, f32, f32)>,
}

pub fn line(time: &str, r: &Row) -> String {
    let s = r.smart.map_or(",,,".to_string(), |(p, l, h, t)| format!("{p},{l},{h:.1},{t:.0}"));
    format!(
        "{time},{},{},{},{:.0},{:.0},{},{},{},{},{s}",
        r.mode, r.cpu_c, r.gpu_c, r.cpu_load, r.gpu_load, r.fan_target_pct, r.rpm, u8::from(r.on_battery), u8::from(r.boost)
    )
}

/// Of these file names (`YYYY-MM-DD.csv`), the ones to delete so that only
/// the newest `keep` remain. Names that aren't day files are left alone.
pub fn expired(mut names: Vec<String>, keep: usize) -> Vec<String> {
    names.retain(|n| n.len() == 14 && n.ends_with(".csv") && n.as_bytes()[4] == b'-' && n.as_bytes()[7] == b'-');
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.split_off(keep.min(names.len()))
}

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    ms: u16,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocalTime(t: *mut SystemTime);
}

/// Local ("2026-11-17", "11:05:03").
#[cfg(windows)]
fn now() -> (String, String) {
    let mut t = SystemTime::default();
    // SAFETY: valid out-pointer to a SYSTEMTIME.
    unsafe { GetLocalTime(&mut t) };
    (format!("{:04}-{:02}-{:02}", t.year, t.month, t.day), format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second))
}

pub struct Log {
    dir: PathBuf,
    day: String,
}

impl Log {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, day: String::new() }
    }

    /// Appends one row to today's file (errors are ignored: a full disk
    /// must never stop the fan control). On a new day, old files go.
    #[cfg(windows)]
    pub fn append(&mut self, row: &Row) {
        let (day, time) = now();
        let path = self.dir.join(format!("{day}.csv"));
        if day != self.day {
            let _ = std::fs::create_dir_all(&self.dir);
            let names = std::fs::read_dir(&self.dir).map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).collect()).unwrap_or_default();
            for old in expired(names, KEEP_DAYS) {
                let _ = std::fs::remove_file(self.dir.join(old));
            }
            // today's file from an older build has other columns: put the
            // current header in before the first new row
            if let Ok(text) = std::fs::read_to_string(&path)
                && !text.is_empty()
                && !text.lines().any(|l| l == HEADER)
                && let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path)
            {
                let _ = writeln!(f, "{HEADER}");
            }
            self.day = day;
        }
        let new = !path.exists();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            if new {
                let _ = writeln!(f, "{HEADER}");
            }
            let _ = writeln!(f, "{}", line(&time, row));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_matches_the_header() {
        let r = Row { mode: "smart", cpu_c: 71, gpu_c: 60, cpu_load: 12.4, gpu_load: 0.0, fan_target_pct: 40, rpm: 2380, on_battery: true, boost: false, smart: Some((40, 1, 70.6, 72.0)) };
        let l = line("11:05:03", &r);
        assert_eq!(l, "11:05:03,smart,71,60,12,0,40,2380,1,0,40,1,70.6,72");
        assert_eq!(l.split(',').count(), HEADER.split(',').count());
        let auto = line("11:05:08", &Row { mode: "auto", smart: None, ..r });
        assert!(auto.ends_with(",1,0,,,,"), "{auto}");
        assert_eq!(auto.split(',').count(), HEADER.split(',').count());
    }

    #[test]
    fn keeps_only_the_newest_days() {
        let names: Vec<String> = ["2026-11-01.csv", "2026-11-03.csv", "2026-11-02.csv", "notes.txt", "2026-10-30.csv", "backup-2026.csv"].map(String::from).to_vec();
        assert_eq!(expired(names.clone(), 2), ["2026-11-01.csv", "2026-10-30.csv"]);
        assert!(expired(names.clone(), 10).is_empty());
        assert_eq!(expired(names, 0).len(), 4, "only day files are ever deleted");
    }

    #[cfg(windows)]
    #[test]
    fn writes_a_day_file() {
        let dir = std::env::temp_dir().join(format!("ds-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut log = Log::new(dir.clone());
        log.append(&Row { mode: "auto", ..Default::default() });
        log.append(&Row { mode: "smart", smart: Some((50, 2, 75.0, 72.0)), ..Default::default() });
        let file = std::fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], HEADER);
        assert!(lines[2].contains(",smart,") && lines[2].ends_with(",50,2,75.0,72"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
