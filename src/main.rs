//! Command-line front end.
//!
//! Default: talks to the real laptop (PawnIO first, MSI's WinIO driver as a
//! fallback). `--backend pawnio|winio` forces one. `--mock` uses a fake EC.

use std::process::ExitCode;

use dragonslayer::controller::Controller;
use dragonslayer::ec::backend::{EcBackend, EcResult, MockEc};
use dragonslayer::ec::map::{ChargeLimit, FanCurve, FanMode, KbdBacklight, ShiftMode};
use dragonslayer::ec::open_hardware;

const USAGE: &str = "\
usage: dragonslayer-cli [--mock | --backend pawnio|winio] [command]

commands:
  status                         show everything (default)
  dump                           raw hex dump of all 256 EC bytes
  watch [seconds]                report every EC byte that changes (default 120 s)
  stress [n]                     read status n times (default 300), report failures
  sysinfo                        CPU / RAM / GPU / battery health (no EC needed)
  power [name]                   list Windows power plans / switch to one
  mode  eco|comfort|sport|turbo  performance profile
  fan   auto|silent|basic|advanced (auto/silent also restore the factory curve)
  curve factory|quiet|cool|max|a,b,c,d,e,f,g   custom fan curve (Advanced mode)
  webcam on|off                  webcam power
  boost on|off                   Cooler Boost (max fans)
  charge <10-100>                stop charging at this %
  kbd   <0-3>                    keyboard backlight level

Without --mock this talks to your real laptop.";

fn parse_shift(s: &str) -> Option<ShiftMode> {
    Some(match s {
        "eco" => ShiftMode::Eco,
        "comfort" => ShiftMode::Comfort,
        "sport" => ShiftMode::Sport,
        "turbo" => ShiftMode::Turbo,
        _ => return None,
    })
}

fn parse_fan(s: &str) -> Option<FanMode> {
    Some(match s {
        "auto" => FanMode::Auto,
        "silent" => FanMode::Silent,
        "basic" => FanMode::Basic,
        "advanced" => FanMode::Advanced,
        _ => return None,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_lowercase()).collect();
    let mock = args.iter().any(|a| a == "--mock");
    let mut backend = "auto".to_string();
    let mut rest: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mock" => {}
            "--backend" => backend = it.next().cloned().unwrap_or_default(),
            other => rest.push(other),
        }
    }

    if rest.first().is_some_and(|a| *a == "-h" || *a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if rest.first() == Some(&"sysinfo") {
        return sysinfo();
    }
    if rest.first() == Some(&"power") {
        use dragonslayer::power;
        let plans = power::plans();
        if let Some(want) = rest.get(1) {
            let Some(p) = plans.iter().find(|p| p.name.to_lowercase().contains(want)) else {
                return bad("no power plan with that name");
            };
            if let Err(e) = power::activate(&p.guid) {
                return fail(&e);
            }
        }
        let active = power::active();
        for p in power::plans() {
            println!("{} {}", if Some(p.guid) == active { "*" } else { " " }, p.name);
        }
        return ExitCode::SUCCESS;
    }

    if mock {
        run(Ok(MockEc::gf63_idle()), &rest, "[MOCK EC - not your real laptop]".into())
    } else {
        match open_hardware(&backend) {
            Ok(hw) => {
                let label = format!("[LIVE via {}]", hw.name());
                run(Ok(hw), &rest, label)
            }
            Err(e) => fail(&e.to_string()),
        }
    }
}

fn run<B: EcBackend>(backend: EcResult<B>, rest: &[&str], label: String) -> ExitCode {
    let label = label.as_str();
    let mut ctl = match backend.and_then(Controller::new) {
        Ok(c) => c,
        Err(e) => return fail(&e.to_string()),
    };

    let result = match rest {
        [] | ["status"] => Ok(()),
        ["dump"] => {
            return match ctl.dump() {
                Ok(bytes) => {
                    println!("{label}\nEC firmware: {}\n", ctl.firmware());
                    println!("     00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F");
                    for (row, chunk) in bytes.chunks(16).enumerate() {
                        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
                        let ascii: String = chunk
                            .iter()
                            .map(|&b| if b.is_ascii_graphic() { b as char } else { '.' })
                            .collect();
                        println!("{:02X}:  {}  {ascii}", row * 16, hex.join(" "));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e.to_string()),
            };
        }
        ["stress"] => return stress(&mut ctl, 300),
        ["stress", n] => match n.parse() {
            Ok(n) => return stress(&mut ctl, n),
            Err(_) => return bad("stress takes a number"),
        },
        ["watch"] => return watch(&mut ctl, 120),
        ["watch", secs] => match secs.parse() {
            Ok(s) => return watch(&mut ctl, s),
            Err(_) => return bad("watch takes a number of seconds"),
        },
        ["mode", m] => match parse_shift(m) {
            Some(m) => ctl.set_shift_mode(m),
            None => return bad("mode must be eco|comfort|sport|turbo"),
        },
        ["fan", m] => match parse_fan(m) {
            Some(m) => ctl.set_fan_profile(m, None),
            None => return bad("fan must be auto|silent|basic|advanced"),
        },
        ["curve", spec] => {
            let c = match *spec {
                "factory" => Some(FanCurve::FACTORY),
                "quiet" => Some(FanCurve::QUIET),
                "cool" => Some(FanCurve::COOL),
                "max" => Some(FanCurve::MAX),
                s => FanCurve::parse(s),
            };
            match c {
                Some(c) => ctl.set_fan_profile(FanMode::Advanced, Some(c)),
                None => return bad("curve must be factory|quiet|cool|max or 7 rising numbers 20-100, e.g. 38,43,48,54,60,70,85 (last two at least 70,85)"),
            }
        }
        ["webcam", "on"] => ctl.set_webcam(true),
        ["webcam", "off"] => ctl.set_webcam(false),
        ["boost", "on"] => ctl.set_cooler_boost(true),
        ["boost", "off"] => ctl.set_cooler_boost(false),
        ["charge", p] => match p.parse().ok().and_then(ChargeLimit::new) {
            Some(c) => ctl.set_charge_limit(c),
            None => return bad("charge must be a number from 10 to 100"),
        },
        ["kbd", l] => match l.parse().ok().and_then(KbdBacklight::new) {
            Some(k) => ctl.set_kbd_backlight(k),
            None => return bad("kbd must be 0, 1, 2 or 3"),
        },
        _ => return bad("unknown command"),
    };

    if let Err(e) = result {
        return fail(&e.to_string());
    }
    match ctl.status() {
        Ok(s) => {
            let writes = if ctl.can_write() { "enabled" } else { "DISABLED (read-only)" };
            println!("{label}\n{s}\nWrites        : {writes}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}

fn sysinfo() -> ExitCode {
    use dragonslayer::sysinfo as si;
    let mut cpu = si::CpuMeter::default();
    cpu.sample();
    std::thread::sleep(std::time::Duration::from_millis(500));
    println!("CPU usage : {:?}", cpu.sample().map(|v| format!("{v:.0}%")));
    println!("Memory    : {:?}", si::memory().map(|(u, t)| format!("{u:.1} / {t:.1} GiB")));
    println!("Power     : {:?}", si::power());
    println!("GPU       : {:?}", si::gpu());
    println!("Camera    : {}", if si::camera_present() { "Windows sees a camera" } else { "no camera in Windows" });
    match si::battery() {
        Some(b) => {
            println!("Battery   : {b:?}");
            println!("Health    : {:?}", b.health_pct().map(|h| format!("{h:.1}%")));
        }
        None => println!("Battery   : not found"),
    }
    ExitCode::SUCCESS
}

/// Reads the full status `n` times and checks the static firmware string each
/// time, to measure how often a read fails or returns garbage.
fn stress<B: EcBackend>(ctl: &mut Controller<B>, n: u32) -> ExitCode {
    let fw = ctl.firmware().to_string();
    let start = std::time::Instant::now();
    let (mut failed, mut wrong) = (0u32, 0u32);
    let mut first_err = None;
    for _ in 0..n {
        match ctl.status() {
            Ok(s) if s.firmware == fw => {}
            Ok(_) => wrong += 1,
            Err(e) => {
                failed += 1;
                first_err.get_or_insert(e.to_string());
            }
        }
        match ctl.dump() {
            Ok(d) if d[0xA0..0xAC] == *fw.as_bytes().get(..12).unwrap_or(&[]) => {}
            Ok(_) => wrong += 1,
            Err(e) => {
                failed += 1;
                first_err.get_or_insert(e.to_string());
            }
        }
    }
    let ms = start.elapsed().as_millis();
    println!("{n} status reads + {n} full dumps in {ms} ms");
    println!("failed: {failed}   wrong firmware bytes: {wrong}");
    if let Some(e) = first_err {
        println!("first error: {e}");
    }
    if failed == 0 && wrong == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Polls all 256 EC bytes and reports every byte that changes, so we can see
/// what Dragon Center writes when a setting is changed. Read-only.
/// Output goes to the screen and to `watch-log.txt` in the current folder.
fn watch<B: EcBackend>(ctl: &mut Controller<B>, seconds: u64) -> ExitCode {
    use std::io::Write;
    use std::time::{Duration, Instant};

    const MUTE_AFTER: u32 = 8; // sensors change constantly; stop reporting them

    let mut log = std::fs::File::create("watch-log.txt").ok();
    let mut say = |line: String| {
        println!("{line}");
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    };

    let mut prev = match ctl.dump() {
        Ok(d) => d,
        Err(e) => return fail(&e.to_string()),
    };
    let mut changes = [0u32; 256];
    let start = Instant::now();
    say(format!("Watching the EC for {seconds} s. Change settings in Dragon Center now."));
    say("  time   addr  old -> new".to_string());

    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(400));
        let now = match ctl.dump() {
            Ok(d) => d,
            Err(e) => return fail(&e.to_string()),
        };
        for addr in 0..256 {
            if now[addr] == prev[addr] || changes[addr] > MUTE_AFTER {
                continue;
            }
            changes[addr] += 1;
            if changes[addr] > MUTE_AFTER {
                say(format!("{:6.1}s  0x{addr:02X}  (changes constantly - muted)", start.elapsed().as_secs_f32()));
            } else {
                say(format!(
                    "{:6.1}s  0x{addr:02X}  {:02X} -> {:02X}",
                    start.elapsed().as_secs_f32(),
                    prev[addr],
                    now[addr]
                ));
            }
        }
        prev = now;
    }
    say("Done.".to_string());
    ExitCode::SUCCESS
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::FAILURE
}

fn bad(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}
