@echo off
setlocal
if "%~1"=="--dry-run" goto dryrun
if not "%~1"=="" goto usage
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0Install Moya.ps1"
set "result=%errorlevel%"
pause
exit /b %result%
:dryrun
if not "%~2"=="" goto usage
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0Install Moya.ps1" -DryRun
exit /b %errorlevel%
:usage
echo Usage: "Install Moya.cmd" [--dry-run]
exit /b 2
