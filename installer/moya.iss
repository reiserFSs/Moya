; Inno Setup 6 script. Build via scripts/package-windows.ps1, which passes
; /DAppVersion /DExePath /DCcxPath /DOutDir.
; AppId identifies the installation for upgrades and uninstall. NEVER change it.
#define AppId "{C3B1CCED-A737-4E2A-9674-1D865D79C8A4}"

[Setup]
AppId={{#AppId}
AppName=Moya
AppVersion={#AppVersion}
AppPublisher=reiserFSs
AppPublisherURL=https://github.com/reiserFSs/Moya
; Keep this path: the Photoshop plugin's default app path on Windows points here.
DefaultDirName={localappdata}\Programs\Moya
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
; Restart Manager can't close Moya (winit ignores end-session messages); PrepareToInstall and
; InitializeUninstall ask it to quit with WM_CLOSE instead.
CloseApplications=no
LicenseFile=..\LICENSE
SetupIconFile=..\crates\moya-app\assets\Moya.ico
UninstallDisplayIcon={app}\Moya.exe
OutputDir={#OutDir}
OutputBaseFilename=Moya-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: "plugin"; Description: "Install the &Photoshop plugin (requires Photoshop 2024+ and Creative Cloud)"

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; DestName: "Moya.exe"; Flags: ignoreversion
Source: "{#CcxPath}"; DestDir: "{app}"; DestName: "Moya Photoshop Plugin.ccx"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Moya"; Filename: "{app}\Moya.exe"
Name: "{autodesktop}\Moya"; Filename: "{app}\Moya.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Classes\.moya"; ValueType: string; ValueData: "Moya.Project"; Flags: uninsdeletevalue uninsdeletekeyifempty
Root: HKCU; Subkey: "Software\Classes\Moya.Project"; ValueType: string; ValueData: "Moya project"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Moya.Project\DefaultIcon"; ValueType: string; ValueData: """{app}\Moya.exe"",0"
Root: HKCU; Subkey: "Software\Classes\Moya.Project\shell\open\command"; ValueType: string; ValueData: """{app}\Moya.exe"" ""%1"""

[Code]
var
  // Shown on the Finished page. Message boxes at this step can open behind Creative Cloud
  // and make setup look stuck at the end of the progress bar.
  PluginNote: String;

function ProcessRunning(Image: String): Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'),
    '/C tasklist /FI "IMAGENAME eq ' + Image + '" | find /I "' + Image + '"',
    '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

function PhotoshopRunning: Boolean;
begin
  Result := ProcessRunning('Photoshop.exe');
end;

// Asks a running Moya to quit the way its close button does (taskkill without /F sends
// WM_CLOSE), so it shows its own Save / Discard prompt for unsaved work. Windows' Restart
// Manager cannot close it: winit ignores the end-session messages it sends, and replacing the
// running Moya.exe then fails at the end of the install. Returns True once Moya has quit.
function CloseMoya(TimeoutMs: Integer): Boolean;
var
  Code, Waited: Integer;
begin
  Result := not ProcessRunning('Moya.exe');
  if Result then Exit;
  Log('Asking the running Moya to quit');
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM Moya.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Waited := 0;
  while ProcessRunning('Moya.exe') and (Waited < TimeoutMs) do begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  Result := not ProcessRunning('Moya.exe');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  if CloseMoya(15000) then
    Result := ''
  else
    Result := 'Moya is still running, probably showing a Save / Discard prompt for unsaved changes. ' +
      'Save or discard them in Moya and quit it, then click Back and Next to continue.';
end;

function InitializeUninstall: Boolean;
begin
  Result := CloseMoya(15000);
  if not Result then
    SuppressibleMsgBox('Moya is still running. Save or discard your changes in Moya, quit it, then uninstall again.',
      mbError, MB_OK, IDOK);
end;

function PsQuote(S: String): String;
begin
  StringChangeEx(S, '''', '''''', True);
  Result := '''' + S + '''';
end;

// Runs Adobe's plugin installer hidden, killed after TimeoutMs: it can wait on a prompt
// nobody sees. Returns its exit code, 124 on timeout, -1 if it could not run.
function RunUpia(Upi, Ccx: String; TimeoutMs: Integer): Integer;
var
  Script: String;
  Code: Integer;
begin
  // The .ccx path has spaces, so UPIA needs it in double quotes; \" survives the outer
  // -Command "..." quoting as a literal quote.
  Script := '$p = Start-Process -FilePath ' + PsQuote(Upi) +
    ' -ArgumentList ''/install'',' + PsQuote('\"' + Ccx + '\"') + ' -WindowStyle Hidden -PassThru; ' +
    'if (-not $p.WaitForExit(' + IntToStr(TimeoutMs) + ')) { $p.Kill(); exit 124 }; exit $p.ExitCode';
  if Exec('powershell.exe', '-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "' + Script + '"',
    '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Result := Code
  else
    Result := -1;
end;

// Plugin problems never fail or block the app install.
procedure InstallPlugin;
var
  Ccx, Upi: String;
  Code: Integer;
begin
  if PhotoshopRunning then begin
    PluginNote := 'Photoshop was running, so the Photoshop plugin was not installed. ' +
      'Quit Photoshop and double-click "Moya Photoshop Plugin.ccx" in the Moya install folder.';
    Exit;
  end;
  Ccx := ExpandConstant('{app}\Moya Photoshop Plugin.ccx');
  Upi := ExpandConstant('{commoncf}\Adobe\Adobe Desktop Common\RemoteComponents\UPI\UnifiedPluginInstallerAgent\UnifiedPluginInstallerAgent.exe');
  WizardForm.StatusLabel.Caption := 'Installing the Photoshop plugin...';
  WizardForm.FilenameLabel.Caption := '';
  // Windows UPIA uses /install (macOS uses --install). If it isn't available, fails or
  // hangs, open the .ccx so Creative Cloud installs it, like a double-click.
  if FileExists(Upi) then begin
    Code := RunUpia(Upi, Ccx, 120000);
    if Code = 0 then Exit;
    Log('Plugin installer exit code: ' + IntToStr(Code));
  end;
  if ShellExec('open', Ccx, '', '', SW_SHOWNORMAL, ewNoWait, Code) then
    PluginNote := 'To finish installing the Photoshop plugin, approve the prompts in Creative Cloud.'
  else
    PluginNote := 'The Photoshop plugin could not be installed automatically; Moya itself is installed. ' +
      'Double-click "Moya Photoshop Plugin.ccx" in the Moya install folder (Creative Cloud must be installed).';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('plugin') then InstallPlugin;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and (PluginNote <> '') then
    WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10#13#10 + PluginNote;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usDone then
    SuppressibleMsgBox('Moya was removed. Your projects and presets in %APPDATA%\Moya were kept. ' +
      'Remove the Photoshop plugin in Creative Cloud (Plugins > Manage plugins).',
      mbInformation, MB_OK, IDOK);
end;
