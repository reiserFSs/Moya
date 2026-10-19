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
CloseApplications=yes
CloseApplicationsFilter=Moya.exe
RestartApplications=no
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
function PhotoshopRunning: Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'),
    '/C tasklist /FI "IMAGENAME eq Photoshop.exe" | find /I "Photoshop.exe"',
    '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

// Plugin problems never fail the app install.
procedure InstallPlugin;
var
  Ccx, Upi: String;
  Code: Integer;
begin
  if PhotoshopRunning then begin
    SuppressibleMsgBox('Photoshop is running, so the plugin was not installed. ' +
      'Quit Photoshop and double-click "Moya Photoshop Plugin.ccx" in the Moya install folder.',
      mbInformation, MB_OK, IDOK);
    Exit;
  end;
  Ccx := ExpandConstant('{app}\Moya Photoshop Plugin.ccx');
  Upi := ExpandConstant('{commoncf}\Adobe\Adobe Desktop Common\RemoteComponents\UPI\UnifiedPluginInstallerAgent\UnifiedPluginInstallerAgent.exe');
  if FileExists(Upi) then begin
    if (not Exec(Upi, '--install "' + Ccx + '"', '', SW_HIDE, ewWaitUntilTerminated, Code)) or (Code <> 0) then
      SuppressibleMsgBox('Adobe plugin installation failed; Moya itself is installed. ' +
        'Double-click "Moya Photoshop Plugin.ccx" in the Moya install folder to retry.',
        mbError, MB_OK, IDOK);
  end else begin
    ShellExec('open', Ccx, '', '', SW_SHOWNORMAL, ewNoWait, Code);
    SuppressibleMsgBox('Finish the plugin installation in Creative Cloud and approve any prompts.',
      mbInformation, MB_OK, IDOK);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('plugin') then InstallPlugin;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usDone then
    SuppressibleMsgBox('Moya was removed. Your projects and presets in %APPDATA%\Moya were kept. ' +
      'Remove the Photoshop plugin in Creative Cloud (Plugins > Manage plugins).',
      mbInformation, MB_OK, IDOK);
end;
