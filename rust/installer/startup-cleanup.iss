function IsOwnedStartupCommand(const Command, InstallRoot: String): Boolean;
var
  Launcher, Normal, Extended, Value: String;
begin
  Launcher := AddBackslash(InstallRoot) + 'codexbar.exe';
  Normal := '"' + Launcher + '"';
  Extended := '"\\?\' + Launcher + '"';
  Value := Trim(Command);
  Result := (CompareText(Value, Normal + ' --startup') = 0) or
    (CompareText(Value, Extended + ' --startup') = 0) or
    (CompareText(Value, Normal) = 0) or (CompareText(Value, Extended) = 0);
end;

function CleanupInstalledStartup(const Existing: TInstalledBuild;
  const InstallRoot, Version, Commit, Manifest, Payload: String): Boolean;
var
  I: Integer;
  Name, Command, Current: String;
begin
  Result := True;
  if not Existing.Present or not Existing.Readable or (Existing.Version <> Version) then exit;
  if InstallationPolicyError(Existing, InstallRoot, Version, Commit, Manifest, Payload) <> '' then exit;
  for I := 0 to 1 do begin
    if I = 0 then Name := 'CodexBarStableV2' else Name := 'CodexBar';
    if not CleanupReadRun(Name, Command) then begin
      Log('Startup cleanup skipped absent or unreadable value: ' + Name);
      continue;
    end;
    if not IsOwnedStartupCommand(Command, InstallRoot) then continue;
    { Do not delete a value observed changing during the cleanup. The OS has no
      compare-and-delete registry primitive; lifecycle QA must cover concurrency. }
    if not CleanupReadRun(Name, Current) or (Current <> Command) then continue;
    if not CleanupDeleteRun(Name) then begin
      Log('Startup cleanup could not delete owned value: ' + Name);
      Result := False;
    end;
  end;
end;
