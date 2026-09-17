param(
    [string]$Compiler = "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$runDirectory = Join-Path $repo ("output\installer-tests\run-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runDirectory -Force | Out-Null

# This harness contains no install actions and always aborts initialization.
& $Compiler /Q ("/O" + $runDirectory) (Join-Path $repo 'rust\installer\install-policy-tests.iss')
if ($LASTEXITCODE -ne 0) { throw 'Policy test compilation failed.' }
$log = Join-Path $runDirectory 'policy.log'
$process = Start-Process -FilePath (Join-Path $runDirectory 'install-policy-tests.exe') `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', ('/LOG="' + $log + '"') `
    -WindowStyle Hidden -Wait -PassThru
if ($process.ExitCode -ne 1 -or
    -not (Select-String -LiteralPath $log -SimpleMatch 'POLICY_TESTS passed=29 failed=0' -Quiet)) {
    throw "Policy test failure; see $log"
}

# Compile the real installer with inert test payloads. NEVER execute this output.
& $Compiler /Q ("/O" + $runDirectory) (Join-Path $repo 'rust\installer\startup-cleanup-tests.iss')
if ($LASTEXITCODE -ne 0) { throw 'Cleanup test compilation failed.' }
$cleanupLog = Join-Path $runDirectory 'cleanup.log'
$cleanupProcess = Start-Process -FilePath (Join-Path $runDirectory 'startup-cleanup-tests.exe') `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', ('/LOG="' + $cleanupLog + '"') `
    -WindowStyle Hidden -Wait -PassThru
if ($cleanupProcess.ExitCode -ne 1 -or
    -not (Select-String -LiteralPath $cleanupLog -SimpleMatch 'CLEANUP_TESTS passed=18 failed=0' -Quiet)) {
    throw "Cleanup test failure; see $cleanupLog"
}

# Compile only: the following artifact is not a runnable product build.
foreach ($name in @('codexbar.exe', 'codexbar-cli.exe', 'codexbar-desktop.exe')) {
    Copy-Item -LiteralPath (Join-Path $runDirectory 'install-policy-tests.exe') -Destination (Join-Path $runDirectory $name)
}
$fixture = Join-Path $runDirectory 'install-policy-tests.exe'
& $Compiler /Q '/DAppVersion=0.47.0' ('/DExpectedCommit=' + ('a' * 40)) `
    ('/DExpectedManifestSha256=' + ('b' * 64)) ('/DTargetBinDir=' + $runDirectory) `
    ('/DOutputDir=' + $runDirectory) '/DOutputBaseFilename=DO-NOT-DISTRIBUTE-compile-check' `
    ('/DVCRedistPath=' + $fixture) ('/DWebView2BootstrapperPath=' + $fixture) `
    (Join-Path $repo 'rust\installer\codexbar.iss')
if ($LASTEXITCODE -ne 0) { throw 'Real installer script compilation failed.' }
[pscustomobject]@{
    PolicyTests = '29 passed, 0 failed'
    CleanupTests = '18 passed, 0 failed'
    InstallerScriptCompile = 'passed with inert payloads; not an acceptance build'
    LiveInstallTested = $false
    EvidenceDirectory = $runDirectory
}
