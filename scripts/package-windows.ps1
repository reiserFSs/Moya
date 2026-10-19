[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$ExecutablePath
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT' -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'Package this release from an x64 PowerShell on Windows.'
}
$root = Split-Path $PSScriptRoot -Parent
$manifest = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
$version = [regex]::Match($manifest, '(?ms)^\[workspace\.package\]\s*\r?\n(?:(?!^\[).)*?^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $version -or $version -notmatch '^[0-9A-Za-z.+-]+$') {
    throw 'Cannot read workspace version from Cargo.toml.'
}
if (-not $SkipBuild) {
    Push-Location $root
    try {
        & cargo build --release -p moya-app
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    } finally { Pop-Location }
}
$exe = if ($ExecutablePath) { $ExecutablePath } else { Join-Path $root 'target/release/moya.exe' }
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "Missing $exe. Build moya-app, or use -SkipBuild -ExecutablePath with a crossbuilt x64 executable."
}
$dist = Join-Path $root 'target/release-dist'
$setup = Join-Path $dist "Moya-$version-windows-x64-setup.exe"
$iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
}
if (-not (Test-Path -LiteralPath $iscc)) {
    & choco install innosetup -y --no-progress
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup install failed.' }
}
if (-not (Test-Path -LiteralPath $iscc)) { throw 'Inno Setup 6 (ISCC.exe) not found.' }

Add-Type -AssemblyName System.IO.Compression.FileSystem
$plugin = Join-Path $dist ('plugin-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $plugin | Out-Null
$ccx = Join-Path $dist 'Moya Photoshop Plugin.ccx'
try {
    # Match the macOS package: runtime plugin files only, no development checks.
    foreach ($file in @('manifest.json', 'index.html', 'index.js', 'icons')) {
        Copy-Item -LiteralPath (Join-Path $root "ps-plugin/$file") -Destination $plugin -Recurse
    }
    if (Test-Path -LiteralPath $ccx) { Remove-Item -LiteralPath $ccx -Force }
    [IO.Compression.ZipFile]::CreateFromDirectory($plugin, $ccx)
    if (Test-Path -LiteralPath $setup) { Remove-Item -LiteralPath $setup -Force }
    & $iscc /Qp "/DAppVersion=$version" "/DExePath=$((Resolve-Path -LiteralPath $exe).Path)" "/DCcxPath=$ccx" "/DOutDir=$dist" (Join-Path $root 'installer/moya.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compile failed.' }
} finally {
    Remove-Item -LiteralPath $plugin -Recurse -Force
    Remove-Item -LiteralPath $ccx -Force -ErrorAction SilentlyContinue
}
Get-FileHash -LiteralPath $setup -Algorithm SHA256
Write-Host "Installer: $setup"
