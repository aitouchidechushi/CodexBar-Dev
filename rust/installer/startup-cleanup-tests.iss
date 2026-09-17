[Setup]
AppName=CodexBar isolated startup cleanup tests
AppVersion=1.0.0
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
OutputDir=..\..\output\installer-tests
OutputBaseFilename=startup-cleanup-tests

[Code]
#include "install-policy.iss"
var
  Value: String;
  TargetName: String;
  ReadAllowed, DeleteAllowed, ChangeDuringRead: Boolean;
  Reads, Deletes, Passed, Failed: Integer;

function CleanupReadRun(const Name: String; var Command: String): Boolean;
begin
  Result := False;
  if (Name <> TargetName) or not ReadAllowed then exit;
  Reads := Reads + 1;
  if ChangeDuringRead and (Reads = 2) then Value := '"C:\Other\other.exe"';
  Command := Value;
  Result := True;
end;
function CleanupDeleteRun(const Name: String): Boolean;
begin
  if Name <> TargetName then RaiseException('Unrelated startup value deleted');
  Deletes := Deletes + 1;
  Result := DeleteAllowed;
  if Result then Value := '';
end;
#include "startup-cleanup.iss"

function Fixture(): TInstalledBuild;
begin
  Result.Present := True; Result.Readable := True; Result.HasFiles := True;
  Result.Schema := '1'; Result.Root := 'C:\Test Space\Programs\CodexBar\v2';
  Result.Launcher := Result.Root + '\codexbar.exe';
  Result.AppId := '{6F73B2E3-8E5B-4A2D-9E7B-0C46A0B2F001}';
  Result.Version := '0.47.0'; Result.Commit := StringOfChar('a', 40);
  Result.Manifest := StringOfChar('b', 64); Result.Payload := StringOfChar('e', 64);
end;

procedure Check(const Name, Command, Entry: String; const Existing: TInstalledBuild;
  CanRead, CanDelete, Race: Boolean; WantDeletes: Integer; WantSuccess: Boolean);
var
  Success: Boolean;
begin
  Value := Command; TargetName := Entry; ReadAllowed := CanRead;
  DeleteAllowed := CanDelete; ChangeDuringRead := Race; Reads := 0; Deletes := 0;
  Success := CleanupInstalledStartup(Existing, 'C:\Test Space\Programs\CodexBar\v2',
    '0.47.0', StringOfChar('a', 40), StringOfChar('b', 64), StringOfChar('e', 64));
  if (Deletes = WantDeletes) and (Success = WantSuccess) and
     (((Deletes = 0) and (Race or (Value = Command))) or
      ((Deletes > 0) and ((CanDelete and (Value = '')) or (not CanDelete and (Value = Command))))) then begin
    Passed := Passed + 1; Log('PASS ' + Name);
  end else begin
    Failed := Failed + 1; Log('FAIL ' + Name + ': deletes=' + IntToStr(Deletes));
  end;
end;

function InitializeSetup(): Boolean;
var
  E: TInstalledBuild;
  Owned: String;
begin
  E := Fixture(); Owned := '"' + E.Launcher + '" --startup';
  Check('owned v2 entry', Owned, 'CodexBarStableV2', E, True, True, False, 1, True);
  Check('owned entry under legacy name', Owned, 'CodexBar', E, True, True, False, 1, True);
  Check('canonicalized extended path', '"\\?\' + E.Launcher + '" --startup', 'CodexBarStableV2', E, True, True, False, 1, True);
  Check('case insensitive path', Uppercase(Owned), 'CodexBarStableV2', E, True, True, False, 1, True);
  Check('no argument launcher', '"' + E.Launcher + '"', 'CodexBar', E, True, True, False, 1, True);
  Check('foreign executable', '"C:\Other\other.exe"', 'CodexBarStableV2', E, True, True, False, 0, True);
  Check('old flat installation', '"C:\Test Space\Programs\CodexBar\codexbar.exe"', 'CodexBar', E, True, True, False, 0, True);
  Check('future installation', '"C:\Test Space\Programs\CodexBar\v3\codexbar.exe"', 'CodexBarStableV2', E, True, True, False, 0, True);
  Check('custom arguments', '"' + E.Launcher + '" --custom', 'CodexBar', E, True, True, False, 0, True);
  Check('ambiguous unquoted space', E.Launcher + ' --startup', 'CodexBar', E, True, True, False, 0, True);
  Check('unmatched quote', '"' + E.Launcher, 'CodexBar', E, True, True, False, 0, True);
  Check('empty value', '', 'CodexBarStableV2', E, True, True, False, 0, True);
  Check('unreadable or absent entry', Owned, 'CodexBarStableV2', E, False, True, False, 0, True);
  Check('changed before delete', Owned, 'CodexBarStableV2', E, True, True, True, 0, True);
  Check('delete failure reported', Owned, 'CodexBarStableV2', E, True, False, False, 1, False);
  E.Version := '0.48.0';
  Check('newer installation preserved', Owned, 'CodexBarStableV2', E, True, True, False, 0, True);
  E := Fixture(); E.Payload := StringOfChar('f', 64);
  Check('different build preserved', Owned, 'CodexBarStableV2', E, True, True, False, 0, True);
  E := Fixture(); E.Readable := False;
  Check('unverified registration preserved', Owned, 'CodexBarStableV2', E, True, True, False, 0, True);
  Log(Format('CLEANUP_TESTS passed=%d failed=%d', [Passed, Failed]));
  Result := False;
end;
