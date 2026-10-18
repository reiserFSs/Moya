[CmdletBinding()]
param([switch]$DryRun)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This installer requires Windows.' }
$source = Join-Path $PSScriptRoot 'Moya.exe'
$plugin = Join-Path $PSScriptRoot 'Moya Photoshop Plugin.ccx'
if (-not (Test-Path -LiteralPath $source -PathType Leaf) -or
    -not (Test-Path -LiteralPath $plugin -PathType Leaf)) {
    throw 'Keep this installer beside Moya.exe and Moya Photoshop Plugin.ccx.'
}
$destination = Join-Path $env:LOCALAPPDATA 'Programs\Moya'
$exe = Join-Path $destination 'Moya.exe'
$programs = [Environment]::GetFolderPath('Programs')
$shortcutPath = Join-Path $programs 'Moya.lnk'
$classes = 'HKCU:\Software\Classes'
$command = '"{0}" "%1"' -f $exe

if ($DryRun) {
    Write-Host "[dry-run] Copy: $source -> $exe"
    Write-Host "[dry-run] Create Start Menu shortcut: $shortcutPath -> $exe"
    Write-Host "[dry-run] Set HKCU\Software\Classes\.moya -> Moya.Project"
    Write-Host "[dry-run] Set HKCU\Software\Classes\Moya.Project\shell\open\command -> $command"
} else {
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    # Refuse redirected install targets rather than following a junction/symlink.
    foreach ($path in @($destination, $exe)) {
        if ((Test-Path -LiteralPath $path) -and
            ((Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Refusing to overwrite a redirected install target: $path"
        }
    }
    Copy-Item -LiteralPath $source -Destination $exe -Force
    New-Item -ItemType Directory -Path $programs -Force | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $exe
    $shortcut.WorkingDirectory = $destination
    $shortcut.IconLocation = "$exe,0"
    $shortcut.Save()
    New-Item -Path "$classes\.moya" -Force | Out-Null
    Set-Item -LiteralPath "$classes\.moya" -Value 'Moya.Project'
    New-Item -Path "$classes\Moya.Project\DefaultIcon" -Force | Out-Null
    Set-Item -LiteralPath "$classes\Moya.Project" -Value 'Moya project'
    Set-Item -LiteralPath "$classes\Moya.Project\DefaultIcon" -Value ('"{0}",0' -f $exe)
    New-Item -Path "$classes\Moya.Project\shell\open\command" -Force | Out-Null
    Set-Item -LiteralPath "$classes\Moya.Project\shell\open\command" -Value $command
    Write-Host "Installed app: $exe"
}

if (Get-Process -Name Photoshop -ErrorAction SilentlyContinue) {
    Write-Host 'Photoshop is running: the plugin installation was skipped.'
    Write-Host 'Quit Photoshop and rerun Install Moya.cmd to install the plugin.'
    Write-Host 'The app installation is independent of the plugin installation.'
    exit 0
}
$upi = Join-Path ${env:CommonProgramFiles} 'Adobe\Adobe Desktop Common\RemoteComponents\UPI\UnifiedPluginInstallerAgent\UnifiedPluginInstallerAgent.exe'
if (Test-Path -LiteralPath $upi -PathType Leaf) {
    if ($DryRun) {
        Write-Host "[dry-run] Install plugin: `"$upi`" --install `"$plugin`""
    } else {
        $process = Start-Process -FilePath $upi -ArgumentList @('--install', ('"{0}"' -f $plugin)) -Wait -PassThru
        if ($process.ExitCode -ne 0) {
            throw "Adobe plugin installation failed (exit $($process.ExitCode)); the app is installed."
        }
    }
} else {
    if ($DryRun) {
        Write-Host "[dry-run] Open plugin with Creative Cloud: $plugin"
    } else {
        Start-Process -FilePath $plugin
    }
    Write-Host 'Finish the plugin installation in Creative Cloud and approve any prompts.'
}
Write-Host 'Launch Moya from Start. Start Photoshop, then choose Plugins > Moya.'
Write-Host 'If .moya has another user-selected default, choose Moya in Windows Default apps.'
