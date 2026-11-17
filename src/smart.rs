//! Smart fan mode: the app decides the fan speed itself, and keeps learning.
//!
//! Why not a curve: a curve only sees temperature. On this laptop load lifts
//! the CPU 20 C within seconds while the fan needs ~25 s to spin up, and in
//! light use fan speed hardly changes the temperature at all (measured
//! 2026-11-17, docs/how-dragon-center-works.md). So Smart mode
//!
//! - **learns a map**: for each load level and fan speed, the temperature
//!   the laptop settles at. It never stops updating it (old readings fade),
//!   so it follows room temperature, dust and ageing.
//! - **acts on load, not heat**: when load rises it goes straight to the
//!   speed the map says that load needs, before the heat arrives. Short
//!   spikes are ignored; after load ends it keeps going until the stored
//!   heat is gone.
//! - **learns your programs**: which ones are usually heavy, to spin up the
//!   moment one comes to the front.
//!
//! Pure logic (no hardware, no clock), so it is tested against a simulated
//! laptop. The caller feeds one `Sample` a second and writes `curve_for`
//! the returned speed. That curve keeps the factory floor at the two hottest
//! points, so a frozen app can never leave the fan slow on a hot laptop.

use std::collections::HashMap;

use crate::ec::map::FanCurve;

/// Load levels (rows of the map).
pub const LOADS: usize = 5;
pub const LOAD_NAMES: [&str; LOADS] = ["Idle", "Light", "Medium", "Heavy", "Extreme"];
/// Upper edge of each load level but the last, in load-index points.
const LOAD_EDGES: [f32; LOADS - 1] = [8.0, 20.0, 40.0, 70.0];
/// Fan speeds Smart mode uses (columns of the map): 30, 40, ... 100 %.
pub const STEPS: usize = 8;

pub fn step_pct(step: usize) -> u8 {
    30 + 10 * step.min(STEPS - 1) as u8
}

/// Before anything is learned: expected temperature at 50 % fan per load
/// level, and how many degrees +10 % fan buys there. From the measurement
/// (light use: ~1 C per 10 %), the first real Smart readings (idle 68 C and
/// light 72 C at 30 %) and typical thin-laptop behaviour under load.
const PRIOR_AT_50: [f32; LOADS] = [67.0, 70.0, 76.0, 84.0, 90.0];
const PRIOR_GAIN: [f32; LOADS] = [0.8, 1.2, 2.0, 3.0, 3.5];

/// Temperature Smart mode aims to stay under (plugged in, slider centred).
pub const TARGET_C: f32 = 72.0;
const BATTERY_EXTRA_C: f32 = 4.0;
/// Not worth another 10 % of fan for less than this many degrees.
const KNEE_C: f32 = 1.5;
/// Hotter than the map expects for this load and speed by this much: add
/// speed step by step.
const TRIM_OVER_C: f32 = 6.0;
/// Load must stay higher this long to count (ignores spikes).
const UP_SECS: f32 = 3.0;
/// After load drops, keep the speed this long (and until the heat is gone).
const LINGER_SECS: f32 = 30.0;
const LINGER_MAX_SECS: f32 = 90.0;
/// A speed must be held this long before its temperature is learned.
const SETTLE_SECS: f32 = 40.0;
const SAMPLE_EVERY_SECS: f32 = 5.0;
/// How fast old readings fade (per sample).
const FADE: f32 = 0.15;
/// A program counts as known after this much time in front.
const APP_KNOWN_SECS: f32 = 300.0;
const PRESPIN_SECS: f32 = 20.0;
const MAX_APPS: usize = 40;
/// Steady load for this long (and as long since the last try) before it
/// tries one step slower to learn whether the extra speed buys anything.
const PROBE_AFTER_SECS: f32 = 120.0;
/// A slower try ends if it gets this much hotter than the speed above.
const PROBE_MAX_RISE_C: f32 = 4.0;

/// One reading, once a second.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub cpu_temp: f32,
    pub gpu_temp: f32,
    /// percent busy, 0-100
    pub cpu_load: f32,
    pub gpu_load: f32,
    pub on_battery: bool,
    /// the program in front (exe name), if known. Never this app itself.
    pub app: Option<String>,
    /// how much of the whole CPU that program itself is using, percent
    pub app_cpu: f32,
}

/// One cell of the map: the temperature this (load, fan speed) settles at.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cell {
    pub temp: f32,
    /// readings taken (0 = never been here)
    pub n: u32,
    /// the temperature when this cell was first well known (cooling health)
    pub first: f32,
}

impl Cell {
    pub fn known(&self) -> bool {
        self.n >= 3
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AppStat {
    /// seconds seen in front
    pub secs: f32,
    /// its usual busy load index (rises fast, falls slowly)
    pub typical: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub fan_pct: u8,
    /// plain words: what it is doing and why
    pub reason: String,
    pub load_level: usize,
    pub target_c: f32,
    /// the smoothed temperature it steers by
    pub heat_c: f32,
}

#[derive(Debug, Clone, Default)]
pub struct Brain {
    pub map: [[Cell; STEPS]; LOADS],
    pub apps: HashMap<String, AppStat>,
    /// total time spent learning, seconds
    pub learned_secs: f64,

    heat_lp: Option<f32>,
    load_lp: f32,
    level: usize,
    above_secs: f32,
    /// lowest load level seen during the current streak of higher load
    above_min: usize,
    /// "has the temperature stopped moving": slow average now vs 30 s ago
    slow_lp: Option<f32>,
    trend_ref: f32,
    trend_clock: f32,
    settled: bool,
    below_secs: f32,
    rose_secs_ago: f32,
    step: usize,
    stable_secs: f32,
    sample_clock: f32,
    trim: usize,
    hot_secs: f32,
    cool_secs: f32,
    panic_secs: f32,
    prespin: Option<(usize, f32, String)>,
    /// trying one step slower: (load level, speed step)
    probe: Option<(usize, usize)>,
    probe_rest: f32,
    level_secs: f32,
    seen_level: usize,
    last_app: Option<String>,
}

fn level_of(load_index: f32) -> usize {
    LOAD_EDGES.iter().position(|&e| load_index < e).unwrap_or(LOADS - 1)
}

/// The curve to write for a Smart speed: that speed everywhere, except the
/// two hottest points (82 C, 88 C), which keep the factory floor (70, 85 %).
/// So a frozen app can never leave the fan slow on a hot laptop.
///
/// Not an earlier ramp: the first version also stepped up at 73 and 76 C,
/// and on the real laptop the CPU touches 77-81 C for a second even at
/// idle, so the fan wobbled between 1,800 and 2,700 RPM (history log,
/// 2026-11-17). Ignoring such spikes is the brain's job, not the curve's.
pub fn curve_for(pct: u8) -> FanCurve {
    let p = pct.clamp(FanCurve::MIN_PCT, FanCurve::MAX_PCT);
    let mut s = [p; 7];
    for (i, v) in s.iter_mut().enumerate() {
        *v = p.max(FanCurve::floor(i));
    }
    FanCurve::new(s).unwrap_or(FanCurve::FACTORY)
}

impl Brain {
    /// What the map says each fan speed gives at this load level. Learned
    /// cells as they are. Unknown ones: on a straight line between the
    /// learned cells either side; beyond the learned ones, the nearest
    /// learned cell plus the prior's slope; with nothing learned, the prior.
    /// A faster fan is never expected to be hotter.
    pub fn estimates(&self, level: usize) -> [f32; STEPS] {
        estimate_row(&self.map[level], level)
    }
}

/// See `Brain::estimates`: one row of a map (also used to draw it).
pub fn estimate_row(row: &[Cell; STEPS], level: usize) -> [f32; STEPS] {
    {
        let prior = |f: usize| PRIOR_AT_50[level] - PRIOR_GAIN[level] * (f32::from(step_pct(f)) - 50.0) / 10.0;
        let mut est = [0.0; STEPS];
        for f in 0..STEPS {
            let lo = (0..f).rev().find(|&k| row[k].known());
            let hi = (f + 1..STEPS).find(|&k| row[k].known());
            est[f] = match (row[f].known(), lo, hi) {
                (true, _, _) => row[f].temp,
                (_, Some(a), Some(b)) => row[a].temp + (row[b].temp - row[a].temp) * (f - a) as f32 / (b - a) as f32,
                (_, Some(k), None) | (_, None, Some(k)) => row[k].temp + prior(f) - prior(k),
                (_, None, None) => prior(f),
            };
            if f > 0 {
                est[f] = est[f].min(est[f - 1]);
            }
        }
        est
    }
}

impl Brain {
    /// Slowest speed expected to stay under `target` at this load level;
    /// stops climbing where more fan no longer buys anything. Going below
    /// the current speed needs a degree of margin, so it doesn't flip-flop.
    fn choose(&self, level: usize, target: f32) -> usize {
        let est = self.estimates(level);
        let mut f = 0;
        while f + 1 < STEPS {
            let limit = if f < self.step { target - 1.0 } else { target };
            if est[f] <= limit || est[f] - est[f + 1] < KNEE_C {
                break;
            }
            f += 1;
        }
        f
    }

    /// One step (`dt` seconds since the last, normally 1). `bias_c` is the
    /// user's Quieter (+) / Cooler (-) preference in degrees.
    pub fn tick(&mut self, s: &Sample, dt: f32, bias_c: f32) -> Decision {
        let dt = dt.clamp(0.1, 5.0);
        // the temperature to steer by: CPU readings jump around, so smooth
        // them (12 s); the GPU reads steadily and shares the heat pipes
        let lp = self.heat_lp.get_or_insert(s.cpu_temp);
        *lp += (s.cpu_temp - *lp) * (1.0 - (-dt / 12.0).exp());
        let heat = lp.max(s.gpu_temp);
        let target = (TARGET_C + if s.on_battery { BATTERY_EXTRA_C } else { 0.0 } + bias_c).clamp(60.0, 82.0);

        // load index: what is making heat right now (2 s smoothing)
        let load_now = (s.cpu_load + 0.8 * s.gpu_load).clamp(0.0, 100.0);
        self.load_lp += (load_now - self.load_lp) * (1.0 - (-dt / 2.0).exp());
        let raw = level_of(self.load_lp);

        // programs: remember how heavy each usually is; spin up when a
        // known heavy one comes to the front
        if let Some(app) = &s.app {
            let st = self.apps.entry(app.clone()).or_default();
            st.secs += dt;
            // Only load the program itself causes counts. If it is behind
            // at least half of the CPU load, the whole load (GPU too) is its
            // doing; otherwise just its own share. First version blamed the
            // program in front for everything: this app's own window, open
            // during a build, was "learned" as heavy and revved the fan to
            // 90 % each time it was opened (2026-11-17).
            let caused = if s.app_cpu >= 0.5 * s.cpu_load { load_now } else { s.app_cpu.clamp(0.0, 100.0) };
            let rate = if caused > st.typical { 0.05 } else { 0.004 };
            st.typical += (caused - st.typical) * (rate * dt).min(1.0);
            if self.last_app.as_ref() != Some(app) {
                let usual = level_of(st.typical);
                if st.secs >= APP_KNOWN_SECS && usual >= 2 && usual > self.level {
                    self.prespin = Some((usual, PRESPIN_SECS, app.clone()));
                }
            }
            if self.apps.len() > MAX_APPS
                && let Some(least) = self.apps.iter().min_by(|a, b| a.1.secs.total_cmp(&b.1.secs)).map(|(k, _)| k.clone())
            {
                self.apps.remove(&least);
            }
        }
        self.last_app.clone_from(&s.app);

        // load level: up quickly (but not on a spike), down only after the
        // load has stayed low and the stored heat is gone
        self.rose_secs_ago += dt;
        // rising is judged on the unsmoothed load, second by second: a
        // burst shorter than UP_SECS never counts, however big it is
        let instant = level_of(load_now);
        if instant > self.level {
            self.above_min = if self.above_secs == 0.0 { instant } else { self.above_min.min(instant) };
            self.above_secs += dt;
            self.below_secs = 0.0;
            if self.above_secs >= UP_SECS {
                // one level per UP_SECS: a 3-second burst (a program starting)
                // moves the fan one notch, not straight to 90 %; load that
                // keeps going climbs a level every 3 s
                self.level = self.above_min.min(self.level + 1);
                self.above_secs = 0.0;
                self.rose_secs_ago = 0.0;
            }
        } else if raw < self.level {
            self.below_secs += dt;
            self.above_secs = 0.0;
            if self.below_secs >= LINGER_SECS && (heat <= target + 1.0 || self.below_secs >= LINGER_MAX_SECS) {
                self.level = raw;
                self.below_secs = 0.0;
                self.trim = 0;
            }
        } else {
            self.above_secs = 0.0;
            if raw == self.level {
                self.below_secs = 0.0;
            }
        }
        // settled = the slow average moved less than 0.7 C in 30 s
        let slow = self.slow_lp.get_or_insert(heat);
        *slow += (heat - *slow) * (1.0 - (-dt / 30.0).exp());
        self.trend_clock += dt;
        if self.trend_clock >= 30.0 {
            self.settled = (*slow - self.trend_ref).abs() < 0.7;
            self.trend_ref = *slow;
            self.trend_clock = 0.0;
        }
        if let Some((lvl, left, _)) = &mut self.prespin {
            *left -= dt;
            if *left <= 0.0 || self.level >= *lvl {
                self.prespin = None;
            }
        }
        let level = self.prespin.as_ref().map_or(self.level, |p| p.0.max(self.level));

        // speed from the map, plus extra steps if it runs hotter than expected
        let base = self.choose(level, target);
        // Extra speed only when it runs hotter than the MAP expects for
        // this load and speed (a load it can't see, blocked vents), not
        // merely hotter than the target. Real data, 2026-11-17: with the
        // Cooler preference the target (68 C) sat below what this laptop
        // idles at whatever the fan does, and "hotter than the target"
        // crept the fan from 30 % to 70 % at 3 % CPU load.
        let expected = self.estimates(level)[(base + self.trim).min(STEPS - 1)];
        if heat > expected + TRIM_OVER_C && heat > target {
            self.hot_secs += dt;
            self.cool_secs = 0.0;
            if self.hot_secs >= 10.0 && base + self.trim < STEPS - 1 {
                self.trim += 1;
                self.hot_secs = 0.0;
            }
        } else if heat < expected + 2.0 {
            self.cool_secs += dt;
            self.hot_secs = 0.0;
            if self.cool_secs >= 20.0 && self.trim > 0 {
                self.trim -= 1;
                self.cool_secs = 0.0;
            }
        } else {
            // in between: a moment of heat must not add up across spikes
            self.hot_secs = 0.0;
            self.cool_secs = 0.0;
        }
        let mut step = (base + self.trim).min(STEPS - 1);

        // Learning needs data at speeds it doesn't use. Under a steady load
        // whose target even full speed can't reach (by the map), try one
        // step slower. Kept if nearly as cool (the map's knee rule then
        // settles there), dropped if it gets PROBE_MAX_RISE_C hotter. Real
        // data, 2026-11-17 stress test: on Cooler it sat at 100 % for every
        // sustained load and never found out whether 80 % would do.
        if self.level != self.seen_level {
            self.seen_level = self.level;
            self.level_secs = 0.0;
        }
        self.level_secs += dt;
        self.probe_rest += dt;
        let est = self.estimates(level);
        if let Some((pl, ps)) = self.probe {
            let above = est[(ps + 1).min(STEPS - 1)];
            let too_hot = heat > above + PROBE_MAX_RISE_C || heat >= 85.0;
            if too_hot {
                // remember that it was too hot, so it doesn't try again soon
                let c = &mut self.map[pl][ps];
                c.temp = c.temp.max(heat);
                c.n = c.n.max(3);
            }
            if pl != level || too_hot || self.map[pl][ps].known() || self.prespin.is_some() || self.trim > 0 {
                self.probe = None;
                self.probe_rest = 0.0;
            }
        }
        if self.probe.is_none()
            && self.trim == 0
            && self.prespin.is_none()
            && raw == self.level
            && level == self.level
            && self.level_secs >= PROBE_AFTER_SECS
            && self.probe_rest >= PROBE_AFTER_SECS
            && step > 0
            && !self.map[level][step - 1].known()
            && est[STEPS - 1] > target - 1.0
            && heat < 82.0
        {
            self.probe = Some((level, step - 1));
        }
        if let Some((_, ps)) = self.probe {
            step = ps;
        }

        // hard guards, whatever was learned
        self.panic_secs = if s.cpu_temp >= 92.0 { self.panic_secs + dt } else { 0.0 };
        let panic = self.panic_secs >= 3.0;
        if panic {
            step = STEPS - 1;
        } else if heat >= 88.0 {
            step = step.max(STEPS - 2);
        }

        // learn: once a speed has been held long enough at a steady load,
        // record what temperature it gives
        if step == self.step {
            self.stable_secs += dt;
        } else {
            self.step = step;
            self.stable_secs = 0.0;
            self.sample_clock = 0.0;
        }
        if self.stable_secs >= SETTLE_SECS && self.settled && self.prespin.is_none() && raw == self.level && !panic {
            self.sample_clock += dt;
            if self.sample_clock >= SAMPLE_EVERY_SECS {
                self.sample_clock = 0.0;
                self.learned_secs += f64::from(SAMPLE_EVERY_SECS);
                let c = &mut self.map[self.level][step];
                let a = if c.n < 6 { 1.0 / (c.n + 1) as f32 } else { FADE };
                c.temp += (heat - c.temp) * a;
                c.n = (c.n + 1).min(1_000_000);
                if c.n == 20 {
                    c.first = c.temp;
                }
            }
        }

        let pct = step_pct(step);
        let reason = if panic {
            format!("CPU at {:.0}°C: full speed", s.cpu_temp)
        } else if let Some((_, _, app)) = &self.prespin {
            format!("{app} is usually heavy: spinning up early")
        } else if raw < self.level {
            "Load dropped: clearing the stored heat first".to_string()
        } else if self.probe.is_some() {
            format!("{} load is steady: trying {pct}% to learn if slower is enough", LOAD_NAMES[self.level])
        } else if step == STEPS - 1 && heat > target {
            format!("{} load, {heat:.0}°C: full speed, all the fan can do", LOAD_NAMES[self.level])
        } else if self.trim > 0 {
            format!("Hotter than the map expected ({heat:.0}°C): adding speed")
        } else if self.rose_secs_ago < 15.0 && self.level > 0 {
            format!("{} load started: fan up before the heat arrives", LOAD_NAMES[self.level])
        } else if self.map[self.level][step].known() {
            format!("{} load, {heat:.0}°C: {pct}% keeps it there", LOAD_NAMES[self.level])
        } else {
            format!("{} load, {heat:.0}°C: learning what {pct}% does", LOAD_NAMES[self.level])
        };
        Decision { fan_pct: pct, reason, load_level: level, target_c: target, heat_c: heat }
    }

    /// Cells of the map that have been learned.
    pub fn known_cells(&self) -> usize {
        self.map.iter().flatten().filter(|c| c.known()).count()
    }

    /// How much hotter (+) the laptop now runs than when the map was first
    /// learned, averaged over well-known cells. None until there is enough
    /// history. (Room temperature moves this too.)
    pub fn drift_c(&self) -> Option<f32> {
        let d: Vec<f32> = self.map.iter().flatten().filter(|c| c.n >= 60).map(|c| c.temp - c.first).collect();
        (d.len() >= 3).then(|| d.iter().sum::<f32>() / d.len() as f32)
    }

    /// Programs it knows, heaviest first: (name, usual load level, hours seen).
    pub fn known_apps(&self) -> Vec<(String, usize, f32)> {
        let mut v: Vec<_> = self
            .apps
            .iter()
            .filter(|(_, a)| a.secs >= APP_KNOWN_SECS)
            .map(|(n, a)| (n.clone(), level_of(a.typical), a.secs / 3600.0))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.total_cmp(&a.2)));
        v
    }

    // ---- saved between runs: plain text, one fact per line -----------------

    pub fn to_text(&self) -> String {
        let mut out = format!("v2\nlearned_secs {:.0}\n", self.learned_secs);
        for (l, row) in self.map.iter().enumerate() {
            for (f, c) in row.iter().enumerate() {
                if c.n > 0 {
                    out.push_str(&format!("cell {l} {f} {:.2} {} {:.2}\n", c.temp, c.n, c.first));
                }
            }
        }
        for (name, a) in &self.apps {
            if a.secs >= 30.0 {
                out.push_str(&format!("app {:.0} {:.1} {name}\n", a.secs, a.typical));
            }
        }
        out
    }

    /// Reads what `to_text` wrote; lines it doesn't understand are skipped.
    pub fn parse(text: &str) -> Self {
        let mut b = Self::default();
        // v1 files blamed the program in front for all load: their program
        // list is wrong, so it is dropped (the map is kept)
        let old_apps = text.lines().next() == Some("v1");
        for line in text.lines() {
            let mut it = line.splitn(2, ' ');
            let (kind, rest) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            let nums: Vec<f32> = rest.split(' ').map_while(|x| x.parse().ok()).collect();
            match kind {
                "learned_secs" if nums.len() == 1 => b.learned_secs = f64::from(nums[0]),
                "cell" if nums.len() == 5 => {
                    let (l, f) = (nums[0] as usize, nums[1] as usize);
                    let sane = (20.0..=110.0).contains(&nums[2]);
                    if l < LOADS && f < STEPS && sane {
                        b.map[l][f] = Cell { temp: nums[2], n: nums[3] as u32, first: nums[4] };
                    }
                }
                "app" if !old_apps => {
                    let mut p = rest.splitn(3, ' ');
                    if let (Some(Ok(secs)), Some(Ok(typical)), Some(name)) = (p.next().map(str::parse), p.next().map(str::parse), p.next())
                        && !name.is_empty()
                    {
                        b.apps.insert(name.to_string(), AppStat { secs, typical });
                    }
                }
                _ => {}
            }
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in laptop, shaped like the measured one: in light use the
    /// fan barely matters (50 % -> 90 % is about 4 C), load heats the chip
    /// within seconds, and the heatsink takes about a minute to follow.
    struct Laptop {
        room: f32,
        /// 1.0 = clean; lower = dusty / worn (cools worse)
        cooling: f32,
        sink: f32,
        chip: f32,
    }

    impl Laptop {
        fn new() -> Self {
            Self { room: 28.0, cooling: 1.0, sink: 38.0, chip: 62.0 }
        }
        fn step(&mut self, load_index: f32, fan_pct: u8) {
            let watts = 10.0 + 0.4 * load_index;
            let g = (0.375 + 0.0125 * f32::from(fan_pct)) * self.cooling;
            self.sink += (watts - g * (self.sink - self.room)) / 60.0;
            let chip = self.sink + 24.0 + 0.7 * (watts - 10.0);
            self.chip += (chip - self.chip) * 0.4;
        }
    }

    struct Run {
        brain: Brain,
        pc: Laptop,
        fan: u8,
        changes: u32,
        peak: f32,
        last: Option<Decision>,
        /// the Quieter (+) / Cooler (-) preference
        bias: f32,
        /// the load comes from something other than the program in front
        background: bool,
    }

    impl Run {
        fn new(brain: Brain) -> Self {
            Self { brain, pc: Laptop::new(), fan: 30, changes: 0, peak: 0.0, last: None, bias: 0.0, background: false }
        }
        /// `secs` seconds at a CPU load (percent), with an optional program in front.
        fn go(&mut self, secs: u32, cpu_load: f32, app: Option<&str>) {
            for _ in 0..secs {
                self.pc.step(cpu_load, self.fan);
                // the program in front causes the load, unless `background` says something else does
                let app_cpu = if self.background { 1.0 } else { cpu_load };
                let s = Sample { cpu_temp: self.pc.chip, gpu_temp: self.pc.sink + 12.0, cpu_load, app: app.map(String::from), app_cpu, ..Default::default() };
                let d = self.brain.tick(&s, 1.0, self.bias);
                if d.fan_pct != self.fan {
                    self.changes += 1;
                }
                self.fan = d.fan_pct;
                self.peak = self.peak.max(self.pc.chip);
                self.last = Some(d);
            }
        }
    }

    #[test]
    fn light_use_is_quiet_and_steady() {
        let mut r = Run::new(Brain::default());
        r.go(900, 4.0, None);
        assert_eq!(r.fan, 30, "light use needs no more than the slowest speed");
        assert!(r.changes <= 1, "no hunting: {} changes", r.changes);
        assert!(r.pc.chip < TARGET_C, "and it stays under the target: {:.1}", r.pc.chip);
    }

    #[test]
    fn cooler_preference_does_not_creep_at_idle() {
        // Seen on the real laptop: with Cooler the target sits below what
        // idle settles at whatever the fan does. More fan buys nothing
        // there, so the fan must stay put instead of creeping to 70 %.
        let mut r = Run::new(Brain::default());
        r.bias = -4.0;
        r.go(900, 4.0, None);
        assert!(r.pc.chip > TARGET_C - 4.0, "idle ({:.1}) is above the Cooler target", r.pc.chip);
        assert_eq!(r.fan, 30);
        assert!(r.changes <= 1, "{} changes", r.changes);
        // under real load Cooler does ask for more than Balanced
        let mut cool = Run::new(Brain::default());
        cool.bias = -4.0;
        let mut bal = Run::new(Brain::default());
        for run in [&mut cool, &mut bal] {
            run.go(60, 4.0, None);
            run.go(600, 30.0, None);
        }
        assert!(cool.fan >= bal.fan, "Cooler {} % vs Balanced {} %", cool.fan, bal.fan);
    }

    #[test]
    fn under_steady_load_it_finds_a_slower_speed_that_is_nearly_as_cool() {
        // Seen in the stress test: on Cooler it sat at 100 % for every
        // sustained load. In this model 100 % gives 73.9 C at medium load,
        // 90 % 75.1, 80 % 76.4, 70 % 78.0: past 80 % the fan buys < 1.5 C.
        let mut r = Run::new(Brain::default());
        r.bias = -4.0;
        r.go(120, 4.0, None);
        r.go(60, 30.0, None);
        assert_eq!(r.fan, 100, "at first it doesn't know better: full speed");
        r.go(2400, 30.0, None);
        assert!((70..=90).contains(&r.fan), "after 40 min it found a quieter speed: {} %", r.fan);
        assert!(r.pc.chip < 77.5, "and stays nearly as cool: {:.1} C", r.pc.chip);
        assert!(r.brain.map[2].iter().filter(|c| c.known()).count() >= 2, "it measured the speeds it tried");
        // it never tries slower while the load is still changing
        let mut quick = Run::new(Brain::default());
        quick.bias = -4.0;
        for _ in 0..8 {
            quick.go(60, 30.0, None);
            quick.go(40, 60.0, None);
        }
        assert!(quick.brain.probe.is_none());
    }

    #[test]
    fn spikes_do_not_move_the_fan() {
        let mut r = Run::new(Brain::default());
        r.go(120, 4.0, None);
        let before = r.changes;
        for _ in 0..30 {
            r.go(2, 90.0, None); // 2 s burst, twice a minute
            r.go(28, 4.0, None);
        }
        assert_eq!(r.changes, before, "2-second bursts are ignored");
        // (bursts that add up to real heat are not ignored: see the trim)
    }

    #[test]
    fn load_raises_the_fan_before_the_heat() {
        let mut r = Run::new(Brain::default());
        r.go(300, 4.0, None);
        assert_eq!(r.fan, 30);
        r.go(7, 60.0, None);
        assert!(r.fan > 30 && r.fan < 90, "climbing a level every 3 s, not one jump: {} %", r.fan);
        r.go(5, 60.0, None);
        assert!(r.fan >= 80, "12 s into heavy load the fan is already at {} %", r.fan);
        // a curve would still be waiting: the heatsink has hardly warmed
        assert!(r.pc.sink < 48.0, "heatsink only at {:.1}", r.pc.sink);
        assert!(r.last.unwrap().reason.contains("before the heat"));
    }

    #[test]
    fn keeps_going_after_load_until_the_heat_is_gone() {
        let mut r = Run::new(Brain::default());
        r.go(60, 4.0, None);
        r.go(240, 60.0, None);
        let during = r.fan;
        r.go(15, 4.0, None);
        assert_eq!(r.fan, during, "15 s after the load ended the fan is still up");
        assert!(r.last.as_ref().unwrap().reason.contains("stored heat"));
        r.go(300, 4.0, None);
        assert_eq!(r.fan, 30, "and back to quiet once it has cooled");
    }

    #[test]
    fn learns_the_map_and_uses_it() {
        let mut r = Run::new(Brain::default());
        // an afternoon: light use, medium work, light, heavy, ...
        for _ in 0..4 {
            r.go(400, 4.0, None);
            r.go(400, 30.0, None);
            r.go(400, 4.0, None);
            r.go(400, 60.0, None);
        }
        let b = &r.brain;
        assert!(b.known_cells() >= 3, "learned {} cells", b.known_cells());
        assert!(b.learned_secs > 1000.0);
        // what it learned matches the laptop: idle at 30 % fan settles at
        // 28 + 11.6 W / 0.75 + 24 + 1.1 = 68.6 C in the model
        let idle = b.map[0][0];
        assert!(idle.known() && (idle.temp - 68.6).abs() < 1.0, "idle at 30 %: {:.1}", idle.temp);
        // and under medium load it found the speed where more fan stops paying
        let medium = (0..STEPS).filter(|&f| b.map[2][f].known()).max_by_key(|&f| b.map[2][f].n).unwrap();
        assert!((60..=90).contains(&step_pct(medium)), "medium load mostly at {} %", step_pct(medium));
    }

    #[test]
    fn adapts_when_cooling_gets_worse() {
        let mut r = Run::new(Brain::default());
        for _ in 0..6 {
            r.go(300, 4.0, None);
            r.go(600, 30.0, None);
        }
        let clean_speed = r.fan;
        let clean_cells = r.brain.map[2];
        // dust: the same fan speed now moves 30 % less heat
        r.pc.cooling = 0.7;
        for _ in 0..10 {
            r.go(300, 4.0, None);
            r.go(600, 30.0, None);
        }
        let hotter = (0..STEPS).any(|f| r.brain.map[2][f].n > clean_cells[f].n && r.brain.map[2][f].temp > clean_cells[f].temp + 2.0);
        assert!(hotter, "the map followed the laptop getting hotter");
        assert!(r.fan >= clean_speed, "and it did not answer with less fan ({} vs {clean_speed})", r.fan);
        assert!(r.brain.drift_c().is_some_and(|d| d > 1.0), "cooling drift noticed: {:?}", r.brain.drift_c());
    }

    #[test]
    fn remembers_heavy_programs() {
        let mut r = Run::new(Brain::default());
        r.go(120, 4.0, Some("notes.exe"));
        r.go(600, 65.0, Some("game.exe")); // a play session
        r.go(600, 4.0, Some("notes.exe"));
        assert_eq!(r.fan, 30);
        // game comes to the front again; its load hasn't started yet
        r.go(2, 4.0, Some("game.exe"));
        assert!(r.fan >= 70, "spun up for a program it knows is heavy: {} %", r.fan);
        assert!(r.last.as_ref().unwrap().reason.contains("game.exe"));
        // a light program never triggers it
        r.go(300, 4.0, Some("notes.exe"));
        assert_eq!(r.fan, 30);
        assert_eq!(r.brain.known_apps()[0].0, "game.exe");
    }

    #[test]
    fn load_from_elsewhere_does_not_make_the_front_program_heavy() {
        // seen on the real laptop: this app's own window was in front
        // during builds and got "learned" as a heavy program
        let mut r = Run::new(Brain::default());
        r.background = true;
        r.go(900, 65.0, Some("reader.exe")); // a build runs behind a reader
        r.background = false;
        r.go(600, 4.0, Some("notes.exe"));
        assert_eq!(r.fan, 30);
        r.go(3, 4.0, Some("reader.exe"));
        assert_eq!(r.fan, 30, "bringing it to the front does not spin the fan up");
        assert!(r.brain.known_apps().iter().all(|a| a.1 < 2), "{:?}", r.brain.known_apps());
        // and an old (v1) file's program list is not trusted
        let old = Brain::parse("v1
cell 0 0 68.0 9 0
app 900 60 dragonslayer.exe
");
        assert!(old.apps.is_empty() && old.known_cells() == 1);
    }

    #[test]
    fn full_speed_when_dangerously_hot() {
        let mut b = Brain::default();
        let s = Sample { cpu_temp: 94.0, gpu_temp: 70.0, cpu_load: 3.0, ..Default::default() };
        let mut d = b.tick(&s, 1.0, 6.0);
        for _ in 0..4 {
            d = b.tick(&s, 1.0, 6.0);
        }
        assert_eq!(d.fan_pct, 100, "{}", d.reason);
    }

    #[test]
    fn quieter_and_battery_raise_the_target() {
        let mut b = Brain::default();
        let s = Sample { cpu_temp: 60.0, gpu_temp: 55.0, ..Default::default() };
        assert_eq!(b.tick(&s, 1.0, 0.0).target_c, TARGET_C);
        assert_eq!(b.tick(&s, 1.0, 4.0).target_c, TARGET_C + 4.0);
        let bat = Sample { on_battery: true, ..s };
        assert_eq!(b.tick(&bat, 1.0, 0.0).target_c, TARGET_C + BATTERY_EXTRA_C);
    }

    #[test]
    fn curve_is_always_safe() {
        for pct in 0..=120u8 {
            let c = curve_for(pct).speeds();
            assert!(FanCurve::new(c).is_some(), "{pct}: {c:?}");
            let fl = FanCurve::FACTORY.speeds();
            assert!((5..7).all(|i| c[i] >= fl[i]), "{pct}: never below the factory floor when hot");
        }
        assert_eq!(curve_for(30).speeds(), [30, 30, 30, 30, 30, 70, 85], "no steps below 82 C: spikes must not move the fan");
        assert_eq!(curve_for(90).speeds(), [90; 7]);
    }

    #[test]
    fn saved_and_loaded() {
        let mut r = Run::new(Brain::default());
        r.go(600, 4.0, Some("notes.exe"));
        r.go(600, 60.0, Some("my game.exe"));
        let text = r.brain.to_text();
        let back = Brain::parse(&text);
        assert_eq!(back.known_cells(), r.brain.known_cells());
        assert!((back.learned_secs - r.brain.learned_secs).abs() < 1.0);
        assert!(back.apps.contains_key("my game.exe"), "names with spaces survive");
        for (a, b) in back.map.iter().flatten().zip(r.brain.map.iter().flatten()) {
            assert!(a.n == b.n && (a.temp - b.temp).abs() < 0.01);
        }
        // junk is ignored, not fatal
        let junk = Brain::parse("v9\ncell 99 99 500 1 1\ncell x\napp\n\nhello");
        assert_eq!(junk.known_cells(), 0);
    }
}
