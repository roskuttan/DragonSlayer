# Builds the downloadable release: dist\DragonSlayer-<version>-windows-x64.zip
# plus a SHA-256 file.
#
# Rust puts the full paths of the files it compiled into the exe (in error
# messages): C:\Users\<name>\.cargo\..., the project folder, the .pdb path.
# This build rewrites them to neutral names, and refuses to package if the
# user name, the PC name or the project path is still in an exe.
# Usage: build-release.ps1 [-Version v0.1.0]
param([string]$Version)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
if (-not $Version) { $Version = 'v' + ((Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value) }
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
$flags = @(
  "--remap-path-prefix=$cargoHome=cargo",
  "--remap-path-prefix=$rustupHome=rustup",
  "--remap-path-prefix=$root=dragonslayer",
  '-Clink-arg=/PDBALTPATH:%_PDB%'   # only the .pdb file name, not its folder
)
# the encoded form keeps paths with spaces intact
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
$env:CARGO_TARGET_DIR = Join-Path $root 'target\dist'
try {
  Push-Location $root
  # cargo reports progress on stderr: not an error here
  $ErrorActionPreference = 'Continue'
  cargo build --release 2>&1 | ForEach-Object { "$_" } | Select-Object -Last 1
  $ErrorActionPreference = 'Stop'
  if ($LASTEXITCODE -ne 0) { throw 'build failed' }
} finally {
  Pop-Location
  Remove-Item Env:CARGO_ENCODED_RUSTFLAGS, Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
}
$bin = Join-Path $root 'target\dist\release'

# privacy check: nothing that identifies this PC or its owner
# (not the bare folder name: "MSI-Control" is also the app's old settings
# folder, which it still looks for to move settings over)
$private = @($env:USERNAME, $env:COMPUTERNAME, $env:USERPROFILE, $root) | Where-Object { $_ -and $_.Length -ge 3 } | Select-Object -Unique
foreach ($f in 'dragonslayer.exe', 'dragonslayer-cli.exe') {
  $bytes = [IO.File]::ReadAllBytes((Join-Path $bin $f))
  $texts = [Text.Encoding]::ASCII.GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes)
  foreach ($p in $private) {
    $n = ($texts | ForEach-Object { ([regex]::Matches($_, [regex]::Escape($p), 'IgnoreCase')).Count } | Measure-Object -Sum).Sum
    if ($n -gt 0) { throw "$f still contains '$p' ($n times): not packaging it" }
  }
  "$f : no user name, PC name or local path inside"
}

# package
$name = "DragonSlayer-$Version-windows-x64"
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist $name
if (Test-Path $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item (Join-Path $bin 'dragonslayer.exe'), (Join-Path $bin 'dragonslayer-cli.exe') $stage
Copy-Item (Join-Path $root 'vendor\pawnio\LpcACPIEC.bin') $stage
Copy-Item (Join-Path $root 'vendor\pawnio\COPYING') (Join-Path $stage 'COPYING-PawnIO.txt')
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt')
Get-ChildItem (Join-Path $root 'release') -File | Copy-Item -Destination $stage
$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
$sums = foreach ($f in (Get-Item $zip), (Get-Item (Join-Path $stage 'dragonslayer.exe')), (Get-Item (Join-Path $stage 'dragonslayer-cli.exe'))) { "{0}  {1}" -f (Get-FileHash $f.FullName -Algorithm SHA256).Hash.ToLower(), $f.Name }
$sums | Out-File (Join-Path $dist 'SHA256SUMS.txt') -Encoding ascii
"packaged: $zip ({0:N0} KB)" -f ((Get-Item $zip).Length / 1KB)
$sums
