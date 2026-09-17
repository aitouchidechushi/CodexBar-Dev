{ Pure policy shared by the installer and the non-installing regression harness. }
type
  TInstalledBuild = record
    Present: Boolean;
    Readable: Boolean;
    HasFiles: Boolean;
    Schema: String;
    Root: String;
    Launcher: String;
    AppId: String;
    Version: String;
    Commit: String;
    Manifest: String;
    Payload: String;
  end;

function IsBuildHash(const Value: String; RequiredLength: Integer): Boolean;
var
  I: Integer;
begin
  Result := False;
  if Length(Value) <> RequiredLength then exit;
  for I := 1 to Length(Value) do
    if Pos(Lowercase(Value[I]), '0123456789abcdef') = 0 then exit;
  Result := True;
end;

{ Release versions are exactly major.minor.patch, each within the PE Word range.
  Reject prereleases/metadata rather than inventing an ordering for them. }
function ParseReleaseVersion(const Value: String; var Parts: TArrayOfInteger): Boolean;
var
  I, N, Digit, PartStart: Integer;
begin
  Result := False;
  SetArrayLength(Parts, 3);
  N := 0; PartStart := 1;
  for I := 1 to Length(Value) do begin
    if Value[I] = '.' then begin
      if (I = PartStart) or (N >= 2) then exit;
      N := N + 1; PartStart := I + 1;
    end else begin
      Digit := Pos(Value[I], '0123456789') - 1;
      if Digit < 0 then exit;
      if (I > PartStart) and (Value[PartStart] = '0') then exit;
      if Parts[N] > (65535 - Digit) div 10 then exit;
      Parts[N] := Parts[N] * 10 + Digit;
    end;
  end;
  Result := (N = 2) and (PartStart <= Length(Value));
end;

function InstallationPolicyError(const Existing: TInstalledBuild;
  const InstallRoot, IncomingVersion, IncomingCommit, IncomingManifest, IncomingPayload: String): String;
var
  OldParts, NewParts: TArrayOfInteger;
  I, Comparison: Integer;
begin
  Result := 'The package build identity is invalid. Use a numbered release package.';
  if not ParseReleaseVersion(IncomingVersion, NewParts) then exit;
  if not IsBuildHash(IncomingCommit, 40) or not IsBuildHash(IncomingManifest, 64) then exit;
  if not IsBuildHash(IncomingPayload, 64) then exit;
  Result := 'The existing installation cannot be verified. No files were replaced.';
  if not Existing.Readable then exit;
  if not Existing.Present then begin
    if not Existing.HasFiles then Result := '';
    exit;
  end;
  if (Existing.Schema <> '1') or
    (CompareText(Existing.Root, InstallRoot) <> 0) or
    (CompareText(Existing.Launcher, AddBackslash(InstallRoot) + 'codexbar.exe') <> 0) or
    (CompareText(Existing.AppId, '{6F73B2E3-8E5B-4A2D-9E7B-0C46A0B2F001}') <> 0) then exit;
  if not ParseReleaseVersion(Existing.Version, OldParts) then exit;
  if not IsBuildHash(Existing.Commit, 40) or not IsBuildHash(Existing.Manifest, 64) then exit;
  Comparison := 0;
  for I := 0 to 2 do begin
    if NewParts[I] > OldParts[I] then begin Comparison := 1; break; end;
    if NewParts[I] < OldParts[I] then begin Comparison := -1; break; end;
  end;
  if Comparison < 0 then begin
    Result := 'A newer CodexBar version is installed. Downgrade is not allowed.';
    exit;
  end;
  if (Comparison = 0) and
    ((CompareText(Existing.Commit, IncomingCommit) <> 0) or
     (CompareText(Existing.Manifest, IncomingManifest) <> 0) or
     not IsBuildHash(Existing.Payload, 64) or
     (CompareText(Existing.Payload, IncomingPayload) <> 0)) then begin
    Result := 'A different build has this version number. Increase the version before installing.';
    exit;
  end;
  Result := '';
end;
