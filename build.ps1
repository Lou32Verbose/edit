<#
.SYNOPSIS
Builds edit32 and stages the release package in publish/.
#>
[CmdletBinding()]
param(
    [string] $PublishPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = if ($PSScriptRoot) {
    $PSScriptRoot
}
else {
    Split-Path -Parent $PSCommandPath
}

if ([string]::IsNullOrWhiteSpace($PublishPath)) {
    $PublishPath = Join-Path $repoRoot 'publish'
}
elseif (-not [System.IO.Path]::IsPathRooted($PublishPath)) {
    $PublishPath = Join-Path $repoRoot $PublishPath
}

$PublishPath = [System.IO.Path]::GetFullPath($PublishPath)

function Invoke-CheckedCommand {
    param(
        [Parameter(Mandatory)]
        [string] $FilePath,

        [Parameter(ValueFromRemainingArguments)]
        [string[]] $Arguments
    )

    Write-Host "> $FilePath $($Arguments -join ' ')"
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath failed with exit code $LASTEXITCODE."
    }
}

function Get-RustReleaseConfigPath {
    $versionOutput = & rustc --version
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($versionOutput)) {
        throw 'Unable to determine rustc version.'
    }

    if ($versionOutput -notmatch 'rustc\s+(?<major>\d+)\.(?<minor>\d+)') {
        throw "Unable to parse rustc version from '$versionOutput'."
    }

    $major = [int] $Matches.major
    $minor = [int] $Matches.minor
    if ($major -gt 1 -or ($major -eq 1 -and $minor -gt 90)) {
        return Join-Path $repoRoot '.cargo\release-nightly.toml'
    }

    return Join-Path $repoRoot '.cargo\release.toml'
}

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    throw 'cargo was not found on PATH. Install Rust before running this script.'
}

$rustc = Get-Command rustc -ErrorAction SilentlyContinue
if (-not $rustc) {
    throw 'rustc was not found on PATH. Install Rust before running this script.'
}

$releaseConfig = Get-RustReleaseConfigPath
if (-not (Test-Path -LiteralPath $releaseConfig)) {
    throw "Release config not found: $releaseConfig"
}

Push-Location $repoRoot
try {
    Invoke-CheckedCommand -FilePath $cargo.Source -Arguments @('build', '--config', $releaseConfig, '--release', '--bin', 'edit32')

    $releaseDir = Join-Path $repoRoot 'target\release'
    $packageItems = @(
        Join-Path $releaseDir 'edit32.exe'
        Join-Path $releaseDir 'edit32.pdb'
        Join-Path $repoRoot 'LICENSE'
        Join-Path $repoRoot 'README.md'
    )

    foreach ($item in $packageItems) {
        if (-not (Test-Path -LiteralPath $item)) {
            throw "Expected package item not found: $item"
        }
    }

    $resolvedPublishParent = Resolve-Path -LiteralPath (Split-Path -Path $PublishPath -Parent)
    $resolvedRepoRoot = Resolve-Path -LiteralPath $repoRoot
    $repoRootWithSeparator = $resolvedRepoRoot.Path.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    if (-not ($resolvedPublishParent.Path + [System.IO.Path]::DirectorySeparatorChar).StartsWith($repoRootWithSeparator, [StringComparison]::OrdinalIgnoreCase)) {
        throw "PublishPath must be inside the repository: $PublishPath"
    }

    if (Test-Path -LiteralPath $PublishPath) {
        Remove-Item -LiteralPath $PublishPath -Recurse -Force
    }

    New-Item -ItemType Directory -Path $PublishPath -Force | Out-Null
    foreach ($item in $packageItems) {
        Copy-Item -LiteralPath $item -Destination $PublishPath -Force
    }

    Write-Host "Packaged edit32 in $PublishPath"
}
finally {
    Pop-Location
}
