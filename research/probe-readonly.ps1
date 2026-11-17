<#
.SYNOPSIS
  READ-ONLY probe of the MSI_ACPI WMI interface on an MSI GF63 Thin 10UC.

.DESCRIPTION
  Calls only MSI "Get_*" methods. Never calls any Set_* method and never
  touches the WinIO driver. Saves everything it sees to a text report next
  to this script so we can compare it with what Dragon Center shows.

  Default mode calls only the 3 methods the Linux kernel docs describe as
  plain reads:
    Get_WMI   - WMI interface version
    Get_EC    - EC interface flags + EC firmware version string
    Get_Fan   - fan speed readings (sub-feature 0x00)

  -Explore additionally calls the undocumented read methods
  (Get_Temperature, Get_Thermal, Get_Data, Get_Device, Get_Power) with
  sub-feature 0x00 and dumps raw bytes, so we can work out which byte is
  which. These are "Get" calls, but their exact behaviour is not publicly
  documented — that's why they are opt-in.

  Must be run as Administrator (MSI_ACPI refuses non-admin callers).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\probe-readonly.ps1
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\probe-readonly.ps1 -Explore
#>
[CmdletBinding()]
param(
    [switch]$Explore
)

$ErrorActionPreference = 'Stop'
$stamp  = Get-Date -Format 'yyyy-MM-dd_HH-mm-ss'
$report = Join-Path $PSScriptRoot "probe-output_$stamp.txt"
$lines  = New-Object System.Collections.Generic.List[string]

function Out-Both([string]$text = '') {
    Write-Host $text
    $lines.Add($text)
}

function Format-Hex([byte[]]$bytes) {
    if (-not $bytes) { return '(none)' }
    $rows = @()
    for ($i = 0; $i -lt $bytes.Length; $i += 16) {
        $chunk = $bytes[$i..([Math]::Min($i + 15, $bytes.Length - 1))]
        $hex   = ($chunk | ForEach-Object { $_.ToString('X2') }) -join ' '
        $ascii = -join ($chunk | ForEach-Object { if ($_ -ge 32 -and $_ -lt 127) { [char]$_ } else { '.' } })
        $rows += ('  {0:X2}: {1,-47}  {2}' -f $i, $hex, $ascii)
    }
    return ($rows -join [Environment]::NewLine)
}

# ---------------------------------------------------------------- admin check
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host ''
    Write-Host 'This probe needs Administrator rights (MSI_ACPI rejects normal users).' -ForegroundColor Yellow
    Write-Host 'Right-click PowerShell -> "Run as administrator", then run:' -ForegroundColor Yellow
    Write-Host "  powershell -ExecutionPolicy Bypass -File `"$PSCommandPath`"" -ForegroundColor Cyan
    Write-Host ''
    Write-Host 'Nothing was read or changed.'
    exit 1
}

# --------------------------------------------------------------- WMI handles
# On the GF63 Thin 10UC (BIOS E16R5IMS.104) MSI_ACPI reports "Not supported":
# the class is described but the BIOS doesn't back it. Keep going without it.
$acpi = $null; $acpiErr = $null
try {
    $acpi = Get-WmiObject -Namespace 'root\WMI' -Class 'MSI_ACPI' -ErrorAction Stop | Select-Object -First 1
    if (-not $acpi) { $acpiErr = 'no instance' }
} catch {
    $acpiErr = $_.Exception.Message.Trim()
}
$pkgClass = $null
if ($acpi) { $pkgClass = [wmiclass]'root\WMI:Package_32' }

function New-Package([byte]$subFeature = 0) {
    $p = $pkgClass.CreateInstance()
    $b = New-Object byte[] 32          # must always be 32 bytes (ACPI quirk)
    $b[0] = $subFeature
    $p.Bytes = $b
    return $p
}

# Calls one Get_* method. Out-only methods take no input package.
function Invoke-Get([string]$method, [byte]$subFeature = 0, [switch]$OutOnly) {
    if ($method -notlike 'Get_*') { throw "Refusing to call non-Get method '$method'." }
    if (-not $acpi) { return @{ ok = $false; err = "MSI_ACPI unavailable ($acpiErr)"; bytes = $null } }
    try {
        # The PowerShell WMI adapter returns the out-parameters object
        if ($OutOnly) { $r = $acpi.$method() }
        else          { $r = $acpi.$method((New-Package $subFeature)) }
        $data = $r.Data
        if (-not $data) { return @{ ok = $false; err = 'no Data returned'; bytes = $null } }
        return @{ ok = $true; err = $null; bytes = [byte[]]$data.Bytes }
    } catch {
        return @{ ok = $false; err = $_.Exception.Message; bytes = $null }
    }
}

# ------------------------------------------------------------------- header
Out-Both '=================================================================='
Out-Both ' DragonSlayer read-only probe'
Out-Both " Time     : $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
$cs   = Get-CimInstance Win32_ComputerSystem
$bios = Get-CimInstance Win32_BIOS
Out-Both " Model    : $($cs.Model)"
Out-Both " BIOS     : $($bios.SMBIOSBIOSVersion)"
if ($acpi) { Out-Both " MSI_ACPI : $($acpi.InstanceName)  (Active=$($acpi.Active))" }
else       { Out-Both " MSI_ACPI : NOT AVAILABLE ($acpiErr)" }
Out-Both " Mode     : $(if ($Explore) { 'documented reads + EXPLORE' } else { 'documented reads only' })"
Out-Both '=================================================================='

# --------------------------------------------------------------- Get_WMI
Out-Both ''
Out-Both '--- Get_WMI : WMI interface version ---'
$r = Invoke-Get 'Get_WMI' -OutOnly
if ($r.ok) {
    Out-Both (Format-Hex $r.bytes)
    Out-Both ("  status=0x{0:X2}  version={1}.{2}" -f $r.bytes[0], $r.bytes[1], $r.bytes[2])
} else { Out-Both "  FAILED: $($r.err)" }

# ---------------------------------------------------------------- Get_EC
Out-Both ''
Out-Both '--- Get_EC : EC flags + firmware version ---'
$r = Invoke-Get 'Get_EC' -OutOnly
$ecFw = $null
if ($r.ok) {
    Out-Both (Format-Hex $r.bytes)
    $flags = $r.bytes[1]
    $ecFw  = (-join ($r.bytes[2..29] | Where-Object { $_ -ge 32 -and $_ -lt 127 } | ForEach-Object { [char]$_ })).Trim()
    Out-Both ("  status=0x{0:X2}  flags=0x{1:X2} (binary {2})" -f $r.bytes[0], $flags, [Convert]::ToString($flags, 2).PadLeft(8, '0'))
    Out-Both "  EC firmware string: '$ecFw'"
    if ($ecFw -like '16R5EMS1.1*') {
        Out-Both '  MATCH: firmware is in the msi-ec CONF_G1_7 family (16R5EMS1.1). Register map applies.'
    } elseif ($ecFw) {
        Out-Both '  NO MATCH with 16R5EMS1.1 -> register map must be re-checked before any writes.'
    }
} else { Out-Both "  FAILED: $($r.err)" }

# --------------------------------------------------------------- Get_Fan
Out-Both ''
Out-Both '--- Get_Fan (sub 0x00) : fan speed readings ---'
$r = Invoke-Get 'Get_Fan' 0
if ($r.ok) {
    Out-Both (Format-Hex $r.bytes)
    Out-Both ("  status=0x{0:X2}" -f $r.bytes[0])
    # up to four 16-bit big-endian readings after the status byte; RPM = 480000 / reading
    for ($i = 0; $i -lt 4; $i++) {
        $hi = $r.bytes[1 + 2 * $i]; $lo = $r.bytes[2 + 2 * $i]
        $raw = ($hi -shl 8) -bor $lo
        $rpm = if ($raw -gt 0) { [int](480000 / $raw) } else { 0 }
        Out-Both ("  fan[{0}] raw=0x{1:X4} ({2,5})  ->  ~{3} RPM" -f $i, $raw, $raw, $rpm)
    }
} else { Out-Both "  FAILED: $($r.err)" }

# ------------------------------------------------------------- explore mode
if ($Explore) {
    $knownMap = @(
        'Known GF63 EC registers (msi-ec CONF_G1_7) to look for in the dumps below:',
        '  0x68 CPU temp (C)     0x71 CPU fan %      0x80 GPU temp (C)   0x89 GPU fan %',
        '  0x98 bit7 CoolerBoost 0xF2 shift: C2 Eco / C1 Comfort / C0 Sport / C4 Turbo',
        '  0xF4 fan: 0D Auto / 1D Silent / 4D Basic / 8D Advanced',
        '  0xEF charge limit (bit7 + %)   0xF3 kbd backlight (0x80 + 0..3)'
    )
    Out-Both ''
    Out-Both '=================== EXPLORE (undocumented Get_* reads) ==================='
    $knownMap | ForEach-Object { Out-Both $_ }
    foreach ($m in 'Get_Temperature', 'Get_Thermal', 'Get_Data', 'Get_Device', 'Get_Power') {
        Out-Both ''
        Out-Both "--- $m (sub 0x00) ---"
        $r = Invoke-Get $m 0
        if ($r.ok) {
            Out-Both (Format-Hex $r.bytes)
            Out-Both ("  status=0x{0:X2}" -f $r.bytes[0])
        } else { Out-Both "  FAILED: $($r.err)" }
        Start-Sleep -Milliseconds 150   # firmware handler isn't thread-safe; don't hammer it
    }
}

# ------------------------------------------- MSI single-value data blocks
# Each of these classes holds one value. Reading a WMI data block is read-only.
Out-Both ''
Out-Both '--- MSI data blocks (one value each) ---'
foreach ($cls in 'MSI_System', 'MSI_CPU', 'MSI_VGA', 'MSI_Power', 'MSI_Device',
                 'MSI_Software', 'MSI_AP', 'MSI_Master_Battery', 'MSI_Slave_Battery') {
    try {
        $objs = @(Get-WmiObject -Namespace 'root\WMI' -Class $cls -ErrorAction Stop)
        if ($objs.Count -eq 0) { Out-Both ("  {0,-18} : no instances" -f $cls); continue }
        foreach ($o in $objs) {
            $vals = $o.Properties | Where-Object { $_.Name -notin 'InstanceName', 'Active' } | ForEach-Object {
                $v = $_.Value
                if ($v -is [array]) { "$($_.Name)=[" + (($v | ForEach-Object { '0x{0:X2}' -f $_ }) -join ' ') + ']' }
                elseif ($null -ne $v) { '{0}={1} (0x{2:X2})' -f $_.Name, $v, ([int64]$v -band 0xFFFF) }
                else { "$($_.Name)=null" }
            }
            Out-Both ("  {0,-18} : {1}   [{2}, Active={3}]" -f $cls, ($vals -join ', '), $o.InstanceName, $o.Active)
        }
    } catch {
        Out-Both ("  {0,-18} : FAILED {1}" -f $cls, $_.Exception.Message.Trim())
    }
}

# ------------------------------------------------- Windows baseline to compare
Out-Both ''
Out-Both '--- Windows-side baseline (for comparison) ---'
try {
    Get-CimInstance -Namespace root\WMI -ClassName MSAcpi_ThermalZoneTemperature -ErrorAction Stop | ForEach-Object {
        Out-Both ("  ACPI thermal zone {0}: {1:N1} C" -f $_.InstanceName, (($_.CurrentTemperature / 10) - 273.15))
    }
} catch { Out-Both '  ACPI thermal zones: unavailable' }
try {
    $bat = Get-CimInstance Win32_Battery -ErrorAction Stop | Select-Object -First 1
    Out-Both "  Battery: $($bat.EstimatedChargeRemaining)%  (status code $($bat.BatteryStatus); 2 = on AC)"
} catch { Out-Both '  Battery: unavailable' }

# ------------------------------------------------------------- checklist
Out-Both ''
Out-Both '--- Please note what Dragon Center shows RIGHT NOW ---'
Out-Both '  [ ] Performance mode (Eco / Comfort / Sport / Turbo):'
Out-Both '  [ ] Fan mode (Auto / Silent / Basic / Advanced) and Cooler Boost on/off:'
Out-Both '  [ ] Battery charge limit setting (100% / 80% / 60%):'
Out-Both '  [ ] Keyboard backlight level:'
Out-Both '  [ ] CPU / GPU temperature and fan speed it displays:'

$lines | Set-Content -Path $report -Encoding UTF8
Write-Host ''
Write-Host "Report saved: $report" -ForegroundColor Green
Write-Host 'Nothing was written to the hardware.' -ForegroundColor Green
