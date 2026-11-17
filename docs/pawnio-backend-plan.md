# Real hardware backend: PawnIO

Why: on this GF63 the BIOS doesn't back `MSI_ACPI` ("Not supported"), so the
WMI route is out. MSI reaches the EC with WinIO (raw access to all physical
memory — a security hole). We use **PawnIO** instead: a signed driver whose
EC module only allows the two EC ports.

## What PawnIO gives us (checked 2026-11-16)

- Installer: `PawnIO_setup.exe` from
  https://github.com/namazso/PawnIO.Setup/releases/latest — "Official" edition
  is digitally signed. Pick Official, not "Unrestricted".
- EC module: **`LpcACPIEC.p`** from https://github.com/namazso/PawnIO.Modules
  (LGPL-2.1-or-later). Exports:
  - `ioctl_pio_read`  — in: `[port]` (1 cell) → out: `[value]` (1 cell)
  - `ioctl_pio_write` — in: `[port, value]` (2 cells) → no output
  - Only ports **0x62** (data) and **0x66** (command/status) are allowed;
    anything else → `STATUS_ACCESS_DENIED`.
  - No timeouts inside the module; the caller handles waiting.
  - Caller should hold the global mutex **`\BaseNamedObjects\Access_EC`**
    (`Global\Access_EC`) — the same lock LibreHardwareMonitor, FanControl,
    etc. use, so we don't collide with them.
- User-mode library: PawnIOLib (now in the main PawnIO repo; old PawnIOLib
  repo archived 2026-02-12). Exact function signatures to be confirmed from
  that repo when implementing (expected: open / load module blob / execute
  ioctl by name with `u64` in/out arrays / close).

## EC protocol we implement ourselves (ACPI standard)

Status port `0x66` bits: `OBF` = bit 0 (data ready to read), `IBF` = bit 1
(EC busy taking input).

Read byte at `addr`:
1. wait IBF = 0 → write `0x80` (RD_EC) to `0x66`
2. wait IBF = 0 → write `addr` to `0x62`
3. wait OBF = 1 → read value from `0x62`

Write `value` to `addr`:
1. wait IBF = 0 → write `0x81` (WR_EC) to `0x66`
2. wait IBF = 0 → write `addr` to `0x62`
3. wait IBF = 0 → write `value` to `0x62`
4. wait IBF = 0

Every wait has a timeout (a few ms); on timeout → `EcError::Io`, never retry
a write blindly. The whole transaction runs while holding `Access_EC`.

## Risks and how we handle them

| Risk | Mitigation |
|---|---|
| Windows' own ACPI EC driver uses the same ports and doesn't take `Access_EC` | Short transactions, timeouts, read-back verify on every write (already in `Controller`) |
| Dragon Center's service poking the EC at the same time | Stop MSI services while testing; uninstall at the end |
| Wrong register map on this firmware | `Controller` refuses all writes unless EC firmware at `0xA0` matches `16R5EMS1.1` |
| Bad value written | Allowlist of addresses + valid values only (unit-tested) |

## Order of work

1. You install PawnIO (Official edition) — needs admin, one time.
2. `PawnIoEc` backend implementing `EcBackend` (read-only first).
3. `msi-control status` on real hardware → compare with Dragon Center.
4. First write: keyboard backlight (harmless, easy to see, easy to undo).
5. Then battery limit, performance mode, fan mode, Cooler Boost.
6. Tray/GUI app. 7. Uninstall Dragon Center.
