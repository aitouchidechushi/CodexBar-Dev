#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$ExpectedVersion,

    [string]$RepoRoot = (Split-Path -Parent $PSScriptRoot)
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-RequiredFile {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RelativePath
    )

    $path = Join-Path -Path $RepoRoot -ChildPath $RelativePath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "required file is missing: $RelativePath"
    }

    return $path
}

function Read-CargoPackageVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RelativePath
    )

    $path = Resolve-RequiredFile -RelativePath $RelativePath
    $content = Get-Content -LiteralPath $path -Raw
    $package = [regex]::Match($content, '(?ms)^\[package\]\s*(?<body>.*?)(?=^\[|\z)')
    if (-not $package.Success) {
        throw "[package] section is missing: $RelativePath"
    }

    $version = [regex]::Match($package.Groups['body'].Value, '(?m)^version\s*=\s*"(?<value>[^"]+)"')
    if (-not $version.Success) {
        throw "package version is missing: $RelativePath"
    }

    return $version.Groups['value'].Value
}

function Read-JsonVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RelativePath
    )

    $path = Resolve-RequiredFile -RelativePath $RelativePath
    $document = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    if ($null -eq $document.version -or [string]::IsNullOrWhiteSpace([string]$document.version)) {
        throw "JSON version is missing: $RelativePath"
    }

    return [string]$document.version
}

function Read-MarketingVersion {
    $relativePath = 'version.env'
    $path = Resolve-RequiredFile -RelativePath $relativePath
    $content = Get-Content -LiteralPath $path -Raw
    $version = [regex]::Match($content, '(?m)^MARKETING_VERSION=(?<value>[^\r\n]+)\s*$')
    if (-not $version.Success) {
        throw "MARKETING_VERSION is missing: $relativePath"
    }

    return $version.Groups['value'].Value.Trim()
}

function Read-LockPackageVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PackageName
    )

    $relativePath = 'Cargo.lock'
    $path = Resolve-RequiredFile -RelativePath $relativePath
    $content = Get-Content -LiteralPath $path -Raw
    $matches = [System.Collections.Generic.List[string]]::new()

    foreach ($package in [regex]::Matches($content, '(?ms)^\[\[package\]\]\s*(?<body>.*?)(?=^\[\[package\]\]|\z)')) {
        $body = $package.Groups['body'].Value
        $name = [regex]::Match($body, '(?m)^name\s*=\s*"(?<value>[^"]+)"')
        if (-not $name.Success -or $name.Groups['value'].Value -ne $PackageName) {
            continue
        }

        $version = [regex]::Match($body, '(?m)^version\s*=\s*"(?<value>[^"]+)"')
        if (-not $version.Success) {
            throw "version is missing for package '$PackageName' in $relativePath"
        }

        $matches.Add($version.Groups['value'].Value)
    }

    if ($matches.Count -ne 1) {
        throw "expected one '$PackageName' package in $relativePath, found $($matches.Count)"
    }

    return $matches[0]
}

$checks = @(
    @{
        Label = 'rust/Cargo.toml [package]'
        Read = { Read-CargoPackageVersion -RelativePath 'rust/Cargo.toml' }
    },
    @{
        Label = 'apps/desktop-tauri/src-tauri/Cargo.toml [package]'
        Read = { Read-CargoPackageVersion -RelativePath 'apps/desktop-tauri/src-tauri/Cargo.toml' }
    },
    @{
        Label = 'apps/desktop-tauri/package.json'
        Read = { Read-JsonVersion -RelativePath 'apps/desktop-tauri/package.json' }
    },
    @{
        Label = 'apps/desktop-tauri/src-tauri/tauri.conf.json'
        Read = { Read-JsonVersion -RelativePath 'apps/desktop-tauri/src-tauri/tauri.conf.json' }
    },
    @{
        Label = 'version.env MARKETING_VERSION'
        Read = { Read-MarketingVersion }
    },
    @{
        Label = 'Cargo.lock codexbar'
        Read = { Read-LockPackageVersion -PackageName 'codexbar' }
    },
    @{
        Label = 'Cargo.lock codexbar-desktop-tauri'
        Read = { Read-LockPackageVersion -PackageName 'codexbar-desktop-tauri' }
    }
)

$failures = [System.Collections.Generic.List[string]]::new()

foreach ($check in $checks) {
    try {
        $actualVersion = & $check.Read
        if ($actualVersion -ne $ExpectedVersion) {
            $message = "$($check.Label): expected $ExpectedVersion, found $actualVersion"
            $failures.Add($message)
            Write-Output "[fail] $message"
        }
        else {
            Write-Output "[ok] $($check.Label): $actualVersion"
        }
    }
    catch {
        $message = "$($check.Label): $($_.Exception.Message)"
        $failures.Add($message)
        Write-Output "[fail] $message"
    }
}

if ($failures.Count -gt 0) {
    Write-Output "Version alignment failed with $($failures.Count) error(s)."
    exit 1
}

Write-Output "All $($checks.Count) version sources are aligned at $ExpectedVersion."
