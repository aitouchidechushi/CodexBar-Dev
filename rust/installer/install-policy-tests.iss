[Setup]
AppName=CodexBar isolated installer policy tests
AppVersion=1.0.0
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
OutputDir=..\..\output\installer-tests
OutputBaseFilename=install-policy-tests

[Code]
#include "install-policy.iss"

var
  Passed: Integer;
  Failed: Integer;
  ProbeStatus: Integer;
  MissingField: String;
  ProbeHasDirectory: Boolean;
  ProbeQueries: Integer;
  ProbeCloses: Integer;

{ Replace only OS reads; execute the production registration loader below. }
function GuardRegOpenKeyEx(Root: LongWord; SubKey: String; Options, Access: LongWord;
  var Handle: LongWord): Longint;
begin
  if (Root <> $80000001) or (Access <> $0101) or
     (SubKey <> 'Software\CodexBar\InstallV2') then RaiseException('Wrong registry read boundary');
  Handle := 123;
  Result := ProbeStatus;
end;
function GuardRegCloseKey(Handle: LongWord): Longint;
begin
  if Handle <> 123 then RaiseException('Wrong handle');
  ProbeCloses := ProbeCloses + 1;
  Result := 0;
end;
function GuardDirectoryExists(const Path: String): Boolean;
begin
  Result := ProbeHasDirectory;
end;
function GuardReadString(const Key, Name: String; var Value: String): Boolean;
begin
  ProbeQueries := ProbeQueries + 1;
  Result := False;
  if Name = MissingField then exit;
  if Name = 'SchemaVersion' then Value := '1'
  else if Name = 'InstallRoot' then Value := 'C:\Test\Programs\CodexBar\v2'
  else if Name = 'LauncherPath' then Value := 'C:\Test\Programs\CodexBar\v2\codexbar.exe'
  else if Name = 'AppId' then Value := '{6F73B2E3-8E5B-4A2D-9E7B-0C46A0B2F001}'
  else if Name = 'InstalledVersion' then Value := '0.46.0'
  else if Name = 'ExpectedCommit' then Value := StringOfChar('a', 40)
  else if Name = 'ExpectedManifestSha256' then Value := StringOfChar('b', 64)
  else if Name = 'ExpectedPayloadSha256' then Value := StringOfChar('e', 64)
  else exit;
  Result := True;
end;
#include "install-registration.iss"

function Fixture(): TInstalledBuild;
begin
  Result.Present := True;
  Result.Readable := True;
  Result.HasFiles := True;
  Result.Schema := '1';
  Result.Root := 'C:\Test\Programs\CodexBar\v2';
  Result.Launcher := Result.Root + '\codexbar.exe';
  Result.AppId := '{6F73B2E3-8E5B-4A2D-9E7B-0C46A0B2F001}';
  Result.Version := '0.46.0';
  Result.Commit := StringOfChar('a', 40);
  Result.Manifest := StringOfChar('b', 64);
  Result.Payload := StringOfChar('e', 64);
end;

procedure Check(const Name: String; const Existing: TInstalledBuild;
  const Version, Commit, Manifest: String; Allow: Boolean);
var
  Error: String;
begin
  Error := InstallationPolicyError(Existing, 'C:\Test\Programs\CodexBar\v2',
    Version, Commit, Manifest, StringOfChar('e', 64));
  if (Error = '') = Allow then begin
    Passed := Passed + 1;
    Log('PASS ' + Name);
  end else begin
    Failed := Failed + 1;
    Log('FAIL ' + Name + ': ' + Error);
  end;
end;

function InitializeSetup(): Boolean;
var
  E: TInstalledBuild;
  C, M: String;
begin
  C := StringOfChar('a', 40);
  M := StringOfChar('b', 64);
  E := Fixture();
  Check('downgrade', E, '0.45.9', C, M, False);
  Check('same version changed commit', E, '0.46.0', StringOfChar('c', 40), M, False);
  Check('same version changed manifest', E, '0.46.0', C, StringOfChar('d', 64), False);
  Check('identical repair', E, '0.46.0', C, M, True);
  E.Payload := StringOfChar('f', 64);
  Check('same source different executable', E, '0.46.0', C, M, False);
  E.Payload := '';
  Check('legacy record cannot prove identical repair', E, '0.46.0', C, M, False);
  Check('legacy record allows numbered upgrade', E, '0.47.0', C, M, True);
  E := Fixture();
  Check('upgrade', E, '0.47.0', StringOfChar('c', 40), M, True);
  E.Version := '0.9.0';
  Check('numeric not lexical', E, '0.10.0', C, M, True);
  E := Fixture(); E.Present := False; E.HasFiles := False;
  Check('first install', E, '0.47.0', C, M, True);
  E.HasFiles := True;
  Check('unregistered existing files', E, '0.47.0', C, M, False);
  E := Fixture(); E.Readable := False;
  Check('unreadable registration', E, '0.47.0', C, M, False);
  E := Fixture(); E.Schema := '2';
  Check('future schema', E, '0.47.0', C, M, False);
  E := Fixture(); E.Root := 'C:\Other';
  Check('foreign root', E, '0.47.0', C, M, False);
  E := Fixture(); E.Launcher := 'C:\Other\codexbar.exe';
  Check('foreign launcher', E, '0.47.0', C, M, False);
  E := Fixture(); E.AppId := 'foreign';
  Check('foreign application', E, '0.47.0', C, M, False);
  E := Fixture(); E.Version := 'invalid';
  Check('invalid installed version', E, '0.47.0', C, M, False);
  E := Fixture();
  Check('prerelease rejected', E, '0.47.0-dev', C, M, False);
  Check('overflow rejected', E, '999999999999.0.0', C, M, False);
  Check('leading zero rejected', E, '0.047.0', C, M, False);
  Check('missing version component', E, '0.47', C, M, False);
  Check('invalid package commit', E, '0.47.0', 'unknown', M, False);
  E.Commit := '';
  Check('missing installed identity', E, '0.47.0', C, M, False);
  ProbeStatus := 2; ProbeHasDirectory := False;
  E := ReadInstalledBuild('C:\Test\Programs\CodexBar\v2');
  Check('loader missing registration', E, '0.47.0', C, M, True);
  ProbeStatus := 5;
  E := ReadInstalledBuild('C:\Test\Programs\CodexBar\v2');
  Check('loader access denied is not absence', E, '0.47.0', C, M, False);
  if (ProbeQueries <> 0) or (ProbeCloses <> 0) then RaiseException('Failed open used a registry handle');
  ProbeStatus := 0; ProbeHasDirectory := True;
  E := ReadInstalledBuild('C:\Test\Programs\CodexBar\v2');
  Check('loader complete registration', E, '0.46.0', C, M, True);
  MissingField := 'ExpectedCommit';
  E := ReadInstalledBuild('C:\Test\Programs\CodexBar\v2');
  Check('loader missing required value', E, '0.47.0', C, M, False);
  MissingField := 'ExpectedPayloadSha256';
  E := ReadInstalledBuild('C:\Test\Programs\CodexBar\v2');
  Check('loader historical upgrade', E, '0.47.0', C, M, True);
  Check('loader historical same version', E, '0.46.0', C, M, False);
  if ProbeCloses <> 3 then RaiseException('Registry probe handle leak');
  Log(Format('POLICY_TESTS passed=%d failed=%d', [Passed, Failed]));
  { This harness has no Files/Registry/Run entries; never start an install. }
  Result := False;
end;
