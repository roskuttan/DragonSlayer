# How MSI Dragon Center works (on this GF63 Thin 10UC)

Reverse-engineered 2026-11-16 from this machine (installed files, WMI schema,
service logs) and cross-checked against the open-source `msi-ec` and
`msi-wmi-platform` Linux drivers. Everything here was gathered **read-only**.

---

## 1. The big picture

```
 ┌──────────────────────────┐
 │ One Dragon Center (UI)   │  .NET app + a pile of API_*.dll plugins
 └────────────┬─────────────┘
              │ encrypted named pipe   ("IO:RMRST", "IO:WMRST", "REG:WRST", "PC:RRST")
 ┌────────────▼─────────────┐
 │ MSIAPService.exe         │  "MSI Foundation Service", runs as SYSTEM
 │ (+ MSI_Central_Service,  │
 │    MSI_Companion_Service)│
 └──────┬─────────────┬─────┘
        │             │
        │ WinIO       │ WMI root\WMI : MSI_ACPI
        │ kernel drv  │ (Get_EC / Set_EC / Get_Data / Set_Data ...)
        ▼             ▼
 ┌──────────────────────────┐
 │ Embedded Controller (EC) │  a tiny chip with 256 bytes of "RAM"
 │  fans, temps, battery,   │  that run the fans, LEDs, charging,
 │  shift mode, backlight   │  performance modes
 └──────────────────────────┘
```

**The key insight:** Dragon Center is just a fancy front-end that reads and
writes a handful of bytes in the EC. Every feature (Cooler Boost, battery
limit, Eco/Sport/Turbo, fan modes, keyboard backlight) is one byte or one bit
at a fixed address. Everything else is UI, telemetry, updaters and ads.

## 2. Evidence from this machine

| What | Where | What it tells us |
|------|-------|-----------------|
| Services | `C:\Program Files (x86)\MSI\...` | `MSI_Central_Service`, `MSI_Companion_Service`, `MSIAPService` (Foundation) |
| WMI class | `root\WMI\MSI_ACPI` | 31 methods: `Get_EC/Set_EC`, `Get_Data/Set_Data`, `Get_Fan/Set_Fan`, `Get_MasterBattery`, `Get_Temperature`, ... |
| Buffer type | `root\WMI\Package_32` | Each method takes one 32-byte array (`Bytes`) |
| `MSIAPService_Log.txt` | Foundation Service dir | `loadWinIO` → loads a raw I/O kernel driver; `ReadMemory Address: FC0008EF`, `FC000898`, `FC0008A4` |
| Named-pipe commands | same log | `IO:RMRST` (read memory), `IO:WMRST` (write memory), `REG:WRST` (registry write), `PC:RRST` — UI → service IPC, encrypted |
| `Fan.txt` | Foundation Service dir | Model → fan class table. `GF63 Thin 10UC:1` (class 1 = basic single-fan control; Titan = 4, Stealth = 3) |
| `USB_LED.txt` | Foundation Service dir | GF63 is **not** listed → keyboard light is EC-driven single-zone red, not USB RGB |

### Decoding the log addresses

The EC's 256 bytes are memory-mapped at physical address **`0xFC000800`**.
So the addresses MSI's service was reading are EC registers:

| Logged address | EC register | Meaning (from msi-ec map below) |
|---|---|---|
| `0xFC0008EF` | `0xEF` | Battery charge limit |
| `0xFC000898` | `0x98` | Cooler Boost |
| `0xFC0008A4` | `0xA4` | inside the EC firmware-version string area (0xA0..) |

This independently confirms the community register map for this model.

> **Update 2026-11-16 (first probe run):** on this laptop `MSI_ACPI` returns
> **"Not supported"**, even for non-admin callers, while every other `MSI_*`
> class returns "Access denied" (= exists, needs admin). So this 2021 BIOS
> describes `MSI_ACPI` but doesn't back it, and path 1 below is **not
> available** here. That's why MSIAPService reads the EC through WinIO.
> The other `MSI_*` classes (`MSI_CPU`, `MSI_VGA`, `MSI_Power`,
> `MSI_Master_Battery`, …) are single-value data blocks; the probe now reads
> them to see what they hold. For EC control our app will need a safe,
> signed EC access driver (e.g. PawnIO, used by LibreHardwareMonitor /
> FanControl) instead of WinIO.

## 3. Two hardware paths Dragon Center uses

1. **WMI `MSI_ACPI`** — the firmware's official door. Calls go through ACPI
   in the BIOS, which touches the EC for you. Needs admin. Safest.
2. **WinIO raw memory access** — a kernel driver that lets a user-mode
   program read/write *any* physical memory. MSI uses it to hit the EC window
   at `0xFC000800` directly. Works, but it's a big security hole
   (drivers in this family have had real CVEs) — **our app will not use it.**

### `Package_32` format (WMI calls)

From the Linux `msi-wmi-platform` documentation:

- **Input:** byte 0 = sub-feature / index selector, bytes 1–31 = input data.
  Must always be a full 32 bytes (Windows ACPI quirk).
- **Output:** byte 0 = status (`0x00` = failure), bytes 1–31 = data.
- `Get_EC` returns a flag byte + the EC firmware version string.
- The firmware's ACPI handler is **not thread-safe** → our app must serialize
  all calls (one at a time, behind a lock).

## 4. EC register map — GF63 Thin 10UC

Source: `msi-ec` config `CONF_G1_7`, which lists firmware `16R5EMS1.1`
(our BIOS is `E16R5IMS.104`, same `16R5` board — to be confirmed by reading
the EC firmware string, see §6).

| Feature | Address | Values |
|---|---|---|
| **CPU temp** (°C) | `0x68` | read |
| **CPU fan speed** | `0x71` | read (raw %) |
| **GPU temp** (°C) | `0x80` | read |
| **GPU fan speed** | `0x89` | read (raw %) |
| **Cooler Boost** | `0x98` bit 7 | 1 = max fans |
| **Shift / perf mode** | `0xF2` | `0xC2` Eco · `0xC1` Comfort · `0xC0` Sport · `0xC4` Turbo |
| **Fan mode** | `0xF4` | `0x0D` Auto · `0x1D` Silent · `0x4D` Basic · `0x8D` Advanced |
| **Battery charge limit** | `0xEF` | bit 7 = enabled flag, low 7 bits = stop-charging % (10–100) |
| **Keyboard backlight** | `0xF3` | `0x80` + level (0–3) |
| **Webcam on/off** | `0x2E` bit 1 | (block bit at `0x2F`) |
| **Fn ↔ Win swap** | `0xBF` bit 4 | inverted |
| **EC firmware version** | `0xA0` | ASCII string, ~12 bytes |
| Super Battery | — | not supported on this model |

Example: to cap charging at 60% → write `0x80 | 60` = `0xBC` to `0xEF`.

## 5. What this means for our rebuild

- The whole useful part of Dragon Center = **~10 EC bytes**. A Rust app that
  reads/writes those through WMI replaces it completely.
- We drop: WinIO driver, encrypted pipe, 3 background services, Nahimic,
  LiveUpdate, Game Summary, recovery tools, ~100+ MB of DLLs.
- Read features (temps, fans, mode) are zero-risk. Write features are
  limited to the known values above — no free-form EC poking.

## 5b. Live results (2026-11-16, read through MSI's own driver)

MSI's "WinIO" is stock WinIO 3.x renamed: `KernCoreLib64.sys` (service
`WINIO`, device `\\.\WINIO`, Microsoft-HCP signed) + `KernCoreLib64.dll`
(exports `InitializeKernCoreLib`, `GetPhysLong`, `SetPhysLong`,
`GetPortVal`, `SetPortVal`, …). The device opens **without admin** — any
program on the PC can read physical memory while MSI's service is installed.

Our app (`src/ec/winio.rs`) attaches to that already-running driver and reads
the EC at `0xFC000800 + addr`. First live read:

| Register | Live value | Decodes as | Map correct? |
|---|---|---|---|
| `0xA0..` | `16R5EMS1.102` + `03112021 17:10:24` | firmware, verified family | yes |
| `0x68` / `0x80` | 0x48 / 0x39, moving with load | CPU 72 °C / GPU 57 °C | yes (plausible) |
| `0x71` / `0x89` | 0x30 / 0x2B | fan 48 % / 43 % | yes (plausible) |
| `0xF4` | `0x0D` | fan mode Auto | yes |
| `0xEF` | `0xE4` | charge limit 100 % | yes |
| `0xF3` | `0x80` | keyboard light level 0 | to confirm |
| `0x98` | `0x02` | Cooler Boost off | to confirm |
| `0xF2` | **`0x80`** | **not one of C0/C1/C2/C4** | **NO — needs the watch test** |

Also seen: `0xC8/0xC9` and `0xCD` change constantly (likely fan tachometer),
`0x6A–0x78` / `0x82–0x90` look like the fan-curve temperature/speed tables.

Tool for the open questions: `research/WATCH.bat` (runs
`msi-control watch 180`) logs every EC byte that changes while settings are
clicked in Dragon Center.

### Watch test: Dragon Center "User Scenario" page (2026-11-16)

Each mode was clicked with the mouse (UI Automation `Select()` only moves the
highlight; it does not apply the mode) while `msi-control watch` recorded.

| Dragon Center mode | EC bytes that changed |
|---|---|
| Extreme Performance | `0xF4`: `0D → 8D → 0D` (ends on Auto) |
| Balanced | nothing |
| Silent | `0xF4`: `0D → 9D → 1D` (**fan mode Silent**) |
| Super Battery | `0xF4` back to `0D`; nothing else |
| User | `0x98`: `02 → 82` (**Cooler Boost bit 7 ON**, as configured in the User profile) |

Conclusions:
- **Verified:** `0xF4` fan mode (`0D` Auto, `1D` Silent) and `0x98` bit 7 Cooler Boost.
- **`0xF2` (shift mode) never changed and stays `0x80`.** Dragon Center 2.0.130
  does not use the EC shift-mode register on this laptop. Its performance
  modes are fan mode + something outside the EC (the Windows power scheme
  name did not change either; not yet identified). Writing C0/C1/C2/C4 to
  `0xF2` is untested here — treat as unverified.
- `0xF6` flips `03 ↔ 05` on its own every few seconds (unrelated).
- Not yet tested: keyboard light, battery limit, fan-curve (Advanced) editing.

### Fan curve and webcam tests (2026-11-16, through MSI's WinIO driver)

Fan curve layout (same as the open-source "isw" tool uses for MSI laptops):

| | temperature steps (°C) | fan speeds (%) |
|---|---|---|
| CPU | `0x6A–0x6F`: 55 64 73 76 82 88 | `0x72–0x78`: 38 43 48 54 60 70 85 |
| GPU | `0x82–0x87`: 55 61 67 73 79 85 | `0x8A–0x90`: 38 43 48 54 60 70 85 |

Speed *i* applies once the temperature reaches step *i−1*. In Auto mode the
live fan % matched this table, so Auto uses it too.

- Wrote the "Cool" curve (45 50 56 63 72 82 95) + fan mode `0x8D`: CPU fan
  70→82 %, GPU fan 54→63 % within 3 s, exactly the curve's values for the
  current temperatures (81 °C / 70 °C). Dragon Center did not overwrite it.
- `fan auto` wrote the factory speeds back + `0x0D`: fans returned to 60 / 54 %.
- Webcam: `0x2E` read `0x49` (bit 1 clear = off). Setting bit 1 (`0x4B`)
  stuck, but no camera appeared in Windows after 10 s, and Windows' device
  history has never seen one on this install. Restored to `0x49`. Either the
  camera is disconnected/broken, or this model uses a different control.
  `0x2F` (webcam block) reads `0x0B`, bit 1 set = not blocked.
- Power: this PC runs the "Ultimate Performance" plan, so the Windows 11
  power-mode slider (overlay) is ignored. The app switches whole plans
  instead.

## 6. Still to verify (superseded: the WMI probe is no longer needed)

Script: [`research/probe-readonly.ps1`](../research/probe-readonly.ps1). It calls
only `Get_*` methods (no `Set_*`, no WinIO) and saves a report
`research/probe-output_<time>.txt`. In an **Administrator** PowerShell:

```powershell
cd "D:\MY FILES\PROJECTS\System-Utilities\MSI-Control\research"
powershell -ExecutionPolicy Bypass -File .\probe-readonly.ps1            # documented reads
powershell -ExecutionPolicy Bypass -File .\probe-readonly.ps1 -Explore   # + raw dumps of other Get_* methods
```

Then jot down what Dragon Center currently shows (mode, fan, battery limit,
backlight, temps) so the bytes can be matched to it.

- [ ] Read EC firmware string at `0xA0` — confirm it starts with `16R5EMS1.1`
- [ ] Confirm exact `Package_32` sub-feature byte for reading an arbitrary EC
      register via `Get_Data` / `Get_EC` on this BIOS
- [ ] Read live values of `0x68`, `0xF2`, `0xF4`, `0xEF`, `0xF3` and check they
      match what Dragon Center shows right now
- [ ] Only after all match: try one reversible write (keyboard backlight level)

## Fan measurement and preset tuning (2026-11-17)

`research/measure-fan.ps1` held flat custom curves and logged temperatures,
fan targets and RPM every ~3 s (`research/fan-measure-2026-11-17.csv`).
The laptop was in normal light use, not on a test bench.

| Fan speed | RPM (settled) | Light use: CPU / GPU |
|---|---|---|
| 35 % | 2,080 | 64–67 °C / 60 °C (first 25 s, before a load burst) |
| 45 % | 2,690 | |
| 50 % | 2,990 | 62–66 °C / 59 °C |
| 60 % | 3,570 | |
| 65 % | 3,880 | 62–66 °C / 58 °C |
| 80 % | 4,750 | about 62 °C / 61 °C |
| 90 % | 5,125 (12 s, still rising) | about 60 °C / 60 °C |
| 100 % | 5,770–5,925 | |
| Cooler Boost | 6,860 | |

What it shows:

- **RPM ≈ 59.5 × percent.** Cooler Boost is faster than a 100 % curve.
- **In light use the fan hardly matters.** 50 % and 65 % differ by about
  1 °C, 50 % and 90 % by about 5 °C.
- **The fan is slow, the CPU is fast.** 2,100 → 4,200 RPM takes about 25 s
  (roughly 100 RPM per second, up or down). A load burst lifts the CPU
  20 °C within seconds: 4 busy threads took it to 87 °C in under 3 s even
  with the fan at 100 %, and a burst that began at 35 % peaked at 92 °C
  while the fan was still spinning up. Fan speed changes the temperature
  the CPU settles at, not the first spike.
- **Idle sits on a curve step.** 62–66 °C straddles the 64 °C step, so the
  factory curve flips between 43 % and 48 % (2,560 ↔ 2,860 RPM).

Presets built on that (speeds for <55, 55, 64, 73, 76, 82, 88 °C on the CPU):

| Preset | Curve | Idea |
|---|---|---|
| Quiet | 30 33 33 45 58 70 85 | one low speed (1,960 RPM) for 55–73 °C, then up early because the fan is slow |
| Factory | 38 43 48 54 60 70 85 | MSI's own |
| Cool | 45 52 52 65 75 85 95 | already spinning before load arrives |
| Max | 60 66 66 80 90 100 100 | 100 % from 82 °C |

Not measured: steady temperatures at 30–35 % over many minutes, and any of
the presets under a long game load (the load step ended at once at 87 °C).
If Quiet is too slow for some use, the CPU reaches 73 °C and the curve
speeds up by itself.

## Sources

- msi-ec driver: https://github.com/BeardOverflow/msi-ec (`msi-ec.c`, `CONF_G1_7`)
- Linux MSI WMI Platform docs: https://docs.kernel.org/wmi/devices/msi-wmi-platform.html
