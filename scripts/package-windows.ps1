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
$name = "Moya-$version-windows-x64"
$dist = Join-Path $root 'target/release-dist'
$stage = Join-Path $dist $name
$archive = Join-Path $dist "$name.zip"
# Recreate only this release's staging directory, never an installed application.
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Copy-Item -LiteralPath $exe -Destination (Join-Path $stage 'Moya.exe')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-windows.cmd') -Destination (Join-Path $stage 'Install Moya.cmd')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-windows.ps1') -Destination (Join-Path $stage 'Install Moya.ps1')

Add-Type -AssemblyName System.IO.Compression.FileSystem
$plugin = Join-Path $dist ('plugin-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $plugin | Out-Null
try {
    # Match the macOS package: runtime plugin files only, no development checks.
    foreach ($file in @('manifest.json', 'index.html', 'index.js', 'icons')) {
        Copy-Item -LiteralPath (Join-Path $root "ps-plugin/$file") -Destination $plugin -Recurse
    }
    [IO.Compression.ZipFile]::CreateFromDirectory($plugin, (Join-Path $stage 'Moya Photoshop Plugin.ccx'))
} finally { Remove-Item -LiteralPath $plugin -Recurse -Force }

@"
Moya $version - Windows x64

Requires Windows x64 with a working GPU driver. The optional plugin requires
Photoshop 2024 or later and Adobe Creative Cloud. Windows behavior has not yet
been verified on a real Windows desktop; use the development smoke checklist.

1. Extract the entire $name.zip archive before installing.
2. Quit Photoshop and Moya, then double-click Install Moya.cmd. No admin needed.
   The app installs to %LOCALAPPDATA%\Programs\Moya\Moya.exe, with a per-user
   Start Menu shortcut and .moya project association. If Photoshop is running,
   only the app installs: quit Photoshop and rerun to install the plugin.
3. Approve Creative Cloud's plugin installation if prompted.
4. Launch Moya from Start. In Photoshop, choose Plugins > Moya.
   Only approve Windows security warnings for a release you trust.

Preview without changing files, registry, shortcuts, or launching Adobe:
  "Install Moya.cmd" --dry-run
PowerShell alternative:
  powershell -NoProfile -ExecutionPolicy Bypass -File ".\Install Moya.ps1" -DryRun
The execution policy override applies to this installer process only.

Moya's bridge discovery/token and settings live under %APPDATA%\Moya. Windows
uses inherited user-profile ACLs, not Unix mode 0600. Accept this only when
that profile directory is private to your user, SYSTEM, and administrators;
it does not protect against other processes running as you. Do not grant
other users access or share bridge.json.
Standard Windows profiles are supported. If Photoshop's UXP runtime does not
expose environment variables, plugin discovery falls back to AppData\Roaming
and AppData\Local under your home directory; redirected AppData may not be
discovered. Settings can override the app executable, not bridge discovery.

Uninstall (quit Moya and Photoshop first; no administrator required):
- Remove the Moya plugin using Creative Cloud's plugin manager.
- Delete %LOCALAPPDATA%\Programs\Moya and
  %APPDATA%\Microsoft\Windows\Start Menu\Programs\Moya.lnk.
- In Registry Editor, remove HKEY_CURRENT_USER\Software\Classes\Moya.Project.
  Remove HKEY_CURRENT_USER\Software\Classes\.moya only if its default value
  is still Moya.Project (do not remove another application's association).
- Optionally delete %APPDATA%\Moya to remove settings/presets/discovery data.
  Keep your .moya project files and exported images wherever you saved them.
  If Windows has a separate user-selected default, reset it in Settings >
  Apps > Default apps rather than deleting protected UserChoice registry keys.
"@ | Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding UTF8

if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
[IO.Compression.ZipFile]::CreateFromDirectory($stage, $archive)
Get-FileHash -LiteralPath $archive -Algorithm SHA256
Write-Host "Release archive: $archive"
