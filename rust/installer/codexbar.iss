#define MyAppName "CodexBar"
#ifndef ExpectedCommit
  #error ExpectedCommit must identify the packaged build
#endif
#ifndef ExpectedManifestSha256
  #error ExpectedManifestSha256 must identify the packaged manifests
#endif
#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef TargetBinDir
  #define TargetBinDir "..\\..\\target\\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\\target\\installer"
#endif
#ifndef OutputBaseFilename
  #define OutputBaseFilename "CodexBar-" + AppVersion + "-Setup"
#endif
#ifndef VCRedistPath
  #define VCRedistPath "..\\target\\installer-deps\\vc_redist.x64.exe"
#endif
#ifndef WebView2BootstrapperPath
  #define WebView2BootstrapperPath "..\\target\\installer-deps\\MicrosoftEdgeWebview2Setup.exe"
#endif

[Setup]
AppId={{6F73B2E3-8E5B-4A2D-9E7B-0C46A0B2F001}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppVerName={#MyAppName} {#AppVersion}
AppPublisher=CodexBar Contributors
AppPublisherURL=https://github.com/Finesssee/Win-CodexBar
AppSupportURL=https://github.com/Finesssee/Win-CodexBar/issues
AppUpdatesURL=https://github.com/Finesssee/Win-CodexBar/releases
DefaultDirName={localappdata}\Programs\CodexBar\v2
DefaultGroupName=CodexBar
DisableProgramGroupPage=yes
DisableDirPage=yes
PrivilegesRequired=lowest
UsePreviousAppDir=no
CloseApplications=yes
WizardStyle=modern
Compression=lzma
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
SetupIconFile=..\icons\icon.ico
UninstallDisplayIcon={app}\codexbar.exe
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#TargetBinDir}\codexbar.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#TargetBinDir}\codexbar-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#TargetBinDir}\codexbar-desktop.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\icons\icon.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#VCRedistPath}"; Flags: dontcopy
Source: "{#WebView2BootstrapperPath}"; Flags: dontcopy

[Icons]
Name: "{autoprograms}\CodexBar"; Filename: "{app}\codexbar.exe"; Parameters: "menubar"; WorkingDir: "{app}"; IconFilename: "{app}\icon.ico"
Name: "{autodesktop}\CodexBar"; Filename: "{app}\codexbar.exe"; Parameters: "menubar"; WorkingDir: "{app}"; Tasks: desktopicon; IconFilename: "{app}\icon.ico"

[Registry]
Root: HKCU; Subkey: "Software\CodexBar\InstallV2"; ValueType: string; ValueName: "SchemaVersion"; ValueData: "1"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\CodexBar\InstallV2"; ValueType: string; ValueName: "InstallRoot"; ValueData: "{app}"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\CodexBar\InstallV2"; ValueType: string; ValueName: "LauncherPath"; ValueData: "{app}\codexbar.exe"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\CodexBar\InstallV2"; ValueType: string; ValueName: "AppId"; ValueData: "{{6F73B2E3-8E5B-4A2D-9E7B-0C46A0B2F001}"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\CodexBar\InstallV2"; ValueType: string; ValueName: "InstalledVersion"; ValueData: "{#AppVersion}"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\CodexBar\InstallV2"; ValueType: string; ValueName: "ExpectedCommit"; ValueData: "{#ExpectedCommit}"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\CodexBar\InstallV2"; ValueType: string; ValueName: "ExpectedManifestSha256"; ValueData: "{#ExpectedManifestSha256}"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\codexbar.exe"; Parameters: "menubar"; Description: "Launch CodexBar"; Flags: nowait postinstall skipifsilent; Check: CanLaunchCodexBar

[Code]
var
  NeedsVCRedistRestart: Boolean;
  NeedsWebView2Restart: Boolean;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if CompareText(ExpandConstant('{app}'), ExpandConstant('{localappdata}\Programs\CodexBar\v2')) <> 0 then
    Result := 'CodexBar must be installed in its registered v2 directory. Remove the /DIR override.';
end;

function WebView2InstalledInView(RootKey: Integer): Boolean;
var
  RuntimeVersion: String;
begin
  Result :=
    RegQueryStringValue(
      RootKey,
      'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'pv',
      RuntimeVersion
    ) and
    (RuntimeVersion <> '');
end;

function WebView2NeedsInstall(): Boolean;
begin
  Result :=
    not WebView2InstalledInView(HKLM64) and
    not WebView2InstalledInView(HKLM32) and
    not WebView2InstalledInView(HKCU);
end;

procedure EnsureWebView2Installed();
var
  ResultCode: Integer;
begin
  if not WebView2NeedsInstall() then
    exit;

  ExtractTemporaryFile('MicrosoftEdgeWebview2Setup.exe');

  WizardForm.StatusLabel.Caption := 'Installing Microsoft Edge WebView2 Runtime...';
  WizardForm.ProgressGauge.Style := npbstMarquee;
  try
    if not Exec(
      ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe'),
      '/silent /install',
      '',
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode
    ) then
      RaiseException('Failed to start the Microsoft Edge WebView2 Runtime installer.');

    if (ResultCode <> 0) and (ResultCode <> 1638) and (ResultCode <> 3010) then
      RaiseException(
        'Microsoft Edge WebView2 Runtime installation failed with exit code ' +
        IntToStr(ResultCode) +
        '.'
      );

    if ResultCode = 3010 then
      NeedsWebView2Restart := True;
  finally
    WizardForm.ProgressGauge.Style := npbstNormal;
  end;
end;

function VCRedistInstalledInView(RootKey: Integer): Boolean;
var
  Installed: Cardinal;
begin
  Result :=
    RegQueryDWordValue(
      RootKey,
      'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64',
      'Installed',
      Installed
    ) and
    (Installed = 1);
end;

function VCRedistNeedsInstall(): Boolean;
begin
  Result :=
    not VCRedistInstalledInView(HKLM64) and
    not VCRedistInstalledInView(HKLM32);
end;

procedure EnsureVCRedistInstalled();
var
  ResultCode: Integer;
begin
  if not VCRedistNeedsInstall() then
    exit;

  ExtractTemporaryFile('vc_redist.x64.exe');

  WizardForm.StatusLabel.Caption := 'Installing Microsoft Visual C++ Runtime...';
  WizardForm.ProgressGauge.Style := npbstMarquee;
  try
    if not Exec(
      ExpandConstant('{tmp}\vc_redist.x64.exe'),
      '/install /quiet /norestart',
      '',
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode
    ) then
      RaiseException('Failed to start the Microsoft Visual C++ Runtime installer.');

    if (ResultCode <> 0) and (ResultCode <> 1638) and (ResultCode <> 3010) then
      RaiseException(
        'Microsoft Visual C++ Runtime installation failed with exit code ' +
        IntToStr(ResultCode) +
        '.'
      );

    if ResultCode = 3010 then
      NeedsVCRedistRestart := True;
  finally
    WizardForm.ProgressGauge.Style := npbstNormal;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then begin
    EnsureWebView2Installed();
    EnsureVCRedistInstalled();
  end;
end;

function NeedRestart(): Boolean;
begin
  Result := NeedsVCRedistRestart or NeedsWebView2Restart;
end;

function CanLaunchCodexBar(): Boolean;
begin
  Result := not NeedsVCRedistRestart and not NeedsWebView2Restart;
end;
