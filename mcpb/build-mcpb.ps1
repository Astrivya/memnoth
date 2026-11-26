<#
.SYNOPSIS
    Assemble a one-platform Memnoth .mcpb from pinned release binaries.

.DESCRIPTION
    A release archive contains memnoth(.exe) and memnoth-mcp(.exe). The CLI
    execs memnoth-mcp as a sibling, so both must land in the bundle root. The
    CLI is copied as mn(.exe) to match manifest.json.

    No secret is read, written or embedded by this script.
#>
[CmdletBinding()]
param(
    # Release manifest.json (targets[].triple/filename/sha256), or a pin map:
    #   { "mn.exe": "<sha256>", "memnoth-mcp.exe": "<sha256>" }
    [string]$Manifest,

    # Directory holding the release archives. Defaults to the manifest's directory.
    [string]$ArtifactsDir,

    # An already-extracted mn binary with memnoth-mcp beside it. Unverified mode.
    [string]$Bin,

    # Output .mcpb path. Defaults to .\memnoth-<triple>.mcpb next to this script.
    [string]$Output,

    # Keep the staging directory for inspection.
    [switch]$KeepStage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Template = Join-Path $ScriptDir 'manifest.json'

function Fail([string]$Message) { throw "build-mcpb.ps1: $Message" }

if (-not (Test-Path -LiteralPath $Template -PathType Leaf)) { Fail "missing manifest template: $Template" }
if ([string]::IsNullOrWhiteSpace($Manifest) -and [string]::IsNullOrWhiteSpace($Bin)) {
    Fail 'provide -Manifest or -Bin (see the comment above)'
}
if (-not [string]::IsNullOrWhiteSpace($Manifest) -and -not (Test-Path -LiteralPath $Manifest -PathType Leaf)) {
    Fail "manifest not found: $Manifest"
}
if (-not [string]::IsNullOrWhiteSpace($Bin) -and -not (Test-Path -LiteralPath $Bin -PathType Leaf)) {
    Fail "binary not found: $Bin"
}

# --- platform -> release target triple -------------------------------------
$IsWin = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)
$IsMac = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::OSX)
$BinExt = if ($IsWin) { '.exe' } else { '' }
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$Triple = if ($IsWin) {
    if ($Arch -ne 'X64') { Fail "unsupported Windows architecture for release artifacts: $Arch" }
    'x86_64-pc-windows-msvc'
} elseif ($IsMac) {
    switch ($Arch) {
        'X64'   { 'x86_64-apple-darwin' }
        'Arm64' { 'aarch64-apple-darwin' }
        default { Fail "unsupported macOS architecture: $Arch" }
    }
} else {
    switch ($Arch) {
        'X64'   { 'x86_64-unknown-linux-gnu' }
        'Arm64' { 'aarch64-unknown-linux-gnu' }
        default { Fail "unsupported Linux architecture: $Arch" }
    }
}

$CliName = "mn$BinExt"
$ServerName = "memnoth-mcp$BinExt"
if ([string]::IsNullOrWhiteSpace($Output)) {
    $Output = Join-Path $ScriptDir "memnoth-$Triple.mcpb"
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Extract-Archive([string]$Archive, [string]$Destination) {
    if ($Archive -like '*.zip') {
        Expand-Archive -LiteralPath $Archive -DestinationPath $Destination -Force
    } elseif ($Archive -like '*.tar.gz' -or $Archive -like '*.tgz') {
        & tar -xzf $Archive -C $Destination
        if ($LASTEXITCODE -ne 0) { Fail "tar failed to extract $Archive" }
    } else {
        Fail "unsupported archive format: $Archive (expected .zip or .tar.gz)"
    }
}

# Copy a bare CLI plus its required sibling server into the stage.
function Stage-BarePair([string]$CliPath, [string]$StagePath) {
    if (-not (Test-Path -LiteralPath $CliPath -PathType Leaf)) { Fail "binary not found: $CliPath" }
    $CliDir = Split-Path -Parent (Resolve-Path -LiteralPath $CliPath).Path
    $ServerPath = Join-Path $CliDir $ServerName
    if (-not (Test-Path -LiteralPath $ServerPath -PathType Leaf)) {
        Fail "required sibling server not found: $ServerPath"
    }
    Copy-Item -LiteralPath $CliPath -Destination (Join-Path $StagePath $CliName)
    Copy-Item -LiteralPath $ServerPath -Destination (Join-Path $StagePath $ServerName)
    return $ServerPath
}

# --- assemble ---------------------------------------------------------------
$Stage = Join-Path ([System.IO.Path]::GetTempPath()) ("memnoth-mcpb-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Stage -Force | Out-Null
Copy-Item -LiteralPath $Template -Destination (Join-Path $Stage 'manifest.json')

try {
    if (-not [string]::IsNullOrWhiteSpace($Manifest)) {
        $Data = Get-Content -LiteralPath $Manifest -Raw | ConvertFrom-Json

        if ($null -ne $Data.PSObject.Properties['targets']) {
            $ArtifactRoot = if ([string]::IsNullOrWhiteSpace($ArtifactsDir)) {
                Split-Path -Parent (Resolve-Path -LiteralPath $Manifest).Path
            } else {
                $ArtifactsDir
            }
            $Target = @($Data.targets) | Where-Object { $_.triple -eq $Triple } | Select-Object -First 1
            if ($null -eq $Target) { Fail "no target for $Triple in $Manifest" }
            if ([string]::IsNullOrWhiteSpace($Target.filename) -or [string]::IsNullOrWhiteSpace($Target.sha256)) {
                Fail "target $Triple has no filename/sha256 in $Manifest"
            }
            $Archive = Join-Path $ArtifactRoot $Target.filename
            if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) { Fail "archive not found: $Archive" }

            Write-Host "build-mcpb.ps1: verifying archive $Archive"
            $Actual = Get-Sha256 $Archive
            if ($Actual -ne $Target.sha256.ToLowerInvariant()) {
                Fail "SHA-256 mismatch for $Archive (expected $($Target.sha256), got $Actual)"
            }

            $Unpack = Join-Path $Stage '.unpack'
            New-Item -ItemType Directory -Path $Unpack -Force | Out-Null
            Extract-Archive $Archive $Unpack
            $ReleaseCli = Join-Path $Unpack "memnoth$BinExt"
            $ReleaseServer = Join-Path $Unpack "memnoth-mcp$BinExt"
            if (-not (Test-Path -LiteralPath $ReleaseCli -PathType Leaf)) { Fail "archive has no memnoth$BinExt" }
            if (-not (Test-Path -LiteralPath $ReleaseServer -PathType Leaf)) { Fail "archive has no memnoth-mcp$BinExt" }
            Copy-Item -LiteralPath $ReleaseCli -Destination (Join-Path $Stage $CliName)
            Copy-Item -LiteralPath $ReleaseServer -Destination (Join-Path $Stage $ServerName)
            Remove-Item -LiteralPath $Unpack -Recurse -Force
        } elseif ($null -ne $Data.PSObject.Properties[$CliName]) {
            if ([string]::IsNullOrWhiteSpace($Bin)) { Fail '-Bin is required with a bare-binary pin map' }
            $Actual = Get-Sha256 $Bin
            if ($Actual -ne ([string]$Data.$CliName).ToLowerInvariant()) {
                Fail "SHA-256 mismatch for $Bin (expected $($Data.$CliName), got $Actual)"
            }
            $ServerPath = Stage-BarePair $Bin $Stage
            if ($null -ne $Data.PSObject.Properties[$ServerName]) {
                $ServerActual = Get-Sha256 $ServerPath
                if ($ServerActual -ne ([string]$Data.$ServerName).ToLowerInvariant()) {
                    Fail "SHA-256 mismatch for $ServerPath"
                }
            }
        } else {
            Fail "pin file has neither targets[] nor a '$CliName' sha256 field"
        }
    } else {
        Write-Warning 'UNVERIFIED BUILD - no release manifest was supplied.'
        Write-Warning 'VERIFY BEFORE SHIPPING: check the SHA-256 of both binaries against the pinned release manifest first.'
        Stage-BarePair $Bin $Stage | Out-Null
    }

    Write-Host "build-mcpb.ps1: staging complete ($CliName + $ServerName + manifest.json)"
    Write-Host "build-mcpb.ps1: packing $Stage -> $Output"
    & npx --yes @anthropic-ai/mcpb pack $Stage $Output
    if ($LASTEXITCODE -ne 0) { Fail "mcpb pack exited with $LASTEXITCODE" }
    Write-Host "build-mcpb.ps1: wrote $Output"
}
finally {
    if ($KeepStage) {
        Write-Host "build-mcpb.ps1: staging directory kept at $Stage"
    } else {
        Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
    }
}
