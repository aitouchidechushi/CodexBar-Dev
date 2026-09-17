function ReadInstalledBuild(const InstallRoot: String): TInstalledBuild;
var
  Handle: LongWord;
  Status: Longint;
  Key: String;
begin
  Result.Present := False;
  Result.Readable := False;
  Result.HasFiles := GuardDirectoryExists(InstallRoot);
  Key := 'Software\CodexBar\InstallV2';
  Status := GuardRegOpenKeyEx($80000001, Key, 0, $0101, Handle);
  if Status = 2 then begin
    Result.Readable := True;
    exit;
  end;
  if Status <> 0 then exit;
  GuardRegCloseKey(Handle);
  Result.Present := True;
  Result.Readable :=
    GuardReadString(Key, 'SchemaVersion', Result.Schema) and
    GuardReadString(Key, 'InstallRoot', Result.Root) and
    GuardReadString(Key, 'LauncherPath', Result.Launcher) and
    GuardReadString(Key, 'AppId', Result.AppId) and
    GuardReadString(Key, 'InstalledVersion', Result.Version) and
    GuardReadString(Key, 'ExpectedCommit', Result.Commit) and
    GuardReadString(Key, 'ExpectedManifestSha256', Result.Manifest);
  { Older registrations lack the payload digest: numbered upgrades are safe,
    but the policy must not treat them as proof of an identical repair. }
  Result.Payload := '';
  GuardReadString(Key, 'ExpectedPayloadSha256', Result.Payload);
end;
