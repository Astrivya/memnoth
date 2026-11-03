<#
.SYNOPSIS
    memnoth installer for Windows (PowerShell 5.1+)

.DESCRIPTION
    Downloads, verifies, and installs memnoth CLI from GitHub releases.
    Installs to %LOCALAPPDATA%\memnoth\bin, adds to user PATH, creates 'mn' alias.

.PARAMETER Channel
    Release channel: 'stable', 'beta', 'nightly', 'winget'. Default: 'stable'.
    Use 'winget' to install via Windows Package Manager (winget).

.PARAMETER Version
    Specific version to install (e.g., 'v0.1.0', 'latest'). Default: 'latest'.

.PARAMETER NoVerify
    Skip SHA256 checksum verification. NOT RECOMMENDED.

.PARAMETER InstallDir
    Custom install directory. Default: $env:LOCALAPPDATA\memnoth\bin.

.PARAMETER BaseUrl
    Custom release base URL (for staging/self-hosted). Default: GitHub releases.

.PARAMETER NoPath
    Do not add install directory to user PATH.

.PARAMETER NoAlias
    Do not create 'mn' alias script.

.PARAMETER Force
    Overwrite existing installation without prompting.

.PARAMETER Proxy
    Proxy URI for web requests.

.PARAMETER UseWinget
    Install via Windows Package Manager (winget). Equivalent to -Channel winget.

.EXAMPLE
    # Install latest stable release
    iwr https://memnoth.astrivya.ai/install.ps1 -UseBasicParsing | iex

.EXAMPLE
    # Install specific version
    .\install.ps1 -Version v0.1.0-beta.1

.EXAMPLE
    # Install beta channel
    .\install.ps1 -Channel beta

.EXAMPLE
    # Install to custom directory
    .\install.ps1 -InstallDir "C:\Tools\memnoth"

.EXAMPLE
    # Skip verification (CI/testing only)
    .\install.ps1 -NoVerify

.EXAMPLE
    # Preview what would be installed
    .\install.ps1 -Preview -Version latest

.EXAMPLE
    # Install via winget (recommended for Windows)
    .\install.ps1 -Channel winget

.EXAMPLE
    # Install specific version via winget
    .\install.ps1 -UseWinget -Version v0.1.0-beta.1

.EXAMPLE
    # Preview winget installation
    .\install.ps1 -UseWinget -Preview

[CmdletBinding()]
param(
    [ValidateSet('stable', 'beta', 'nightly', 'winget')]
    [string]$Channel = 'stable',


    [string]$Version = 'latest',

    [switch]$NoVerify,

    [string]$InstallDir = "$env:LOCALAPPDATA\memnoth\bin",

    [string]$BaseUrl = 'https://github.com/astrivya/memnoth/releases/download',

    [switch]$NoPath,

    [switch]$NoAlias,

    [switch]$Force,

    [string]$Proxy,

    [switch]$Preview,

    [switch]$UseWinget
)

# Enable strict mode for better error handling
Set-StrictMode -Version Latest

$ErrorActionPreference = 'Stop'

# Progress preference for cleaner output
$ProgressPreference = 'SilentlyContinue'

# ------------------------------------------------------------ helpers
function Write-Log {
    param(
        [Parameter(Mandatory = $true)][string]$Message,
        [ValidateSet('Info', 'Warn', 'Error', 'Debug')]
        [string]$Level = 'Info'
    )
    $prefix = switch ($Level) {
        'Info'  { '[memnoth]' }
        'Warn'  { '[memnoth] WARN:' }
        'Error' { '[memnoth] ERROR:' }
        'Debug' { '[memnoth] DEBUG:' }
    }
    $color = switch ($Level) {
        'Info'  { 'Cyan' }
        'Warn'  { 'Yellow' }
        'Error' { 'Red' }
        'Debug' { 'Gray' }
    }
    Write-Host "$prefix $Message" -ForegroundColor $color
}

function Fail {
    param([string]$Message)
    Write-Log $Message 'Error'
    exit 1
}

function Get-Sha256 {
    param([string]$Path)
    try {
        $hash = Get-FileHash -Path $Path -Algorithm SHA256 -ErrorAction Stop
        return $hash.Hash.ToLowerInvariant()
    } catch {
        Fail "Failed to compute SHA256 for $Path : $_"
    }
}

function Invoke-Download {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][string]$OutFile,
        [string]$Proxy
    )
    $params = @{
        Uri         = $Url
        OutFile     = $OutFile
        UseBasicParsing = $true
        ErrorAction = 'Stop'
    }
    if ($Proxy) {
        $params.Proxy = $Proxy
        $params.ProxyUseDefaultCredentials = $true
    }
    Write-Log "Downloading $Url"
    try {
        Invoke-WebRequest @params
    } catch {
        $status = 0
        if ($_.Exception.Response) {
            $status = $_.Exception.Response.StatusCode.value__
        }
        if ($status -eq 404) { return 2 }
        Fail "Download failed for $Url : $_"
    }
    return 0
}

function Resolve-Version {
    param([string]$Channel, [string]$Version, [string]$BaseUrl)
    if ($Version -ne 'latest') {
        return $Version
    }
    # For 'latest', we need to determine the latest version for the channel
    # GitHub releases API: latest release per channel is tagged appropriately
    $apiUrl = "https://api.github.com/repos/astrivya/memnoth/releases"
    Write-Log "Resolving latest version for channel '$Channel'..."
    try {
        $releases = Invoke-WebRequest -Uri $apiUrl -UseBasicParsing -ErrorAction Stop | ConvertFrom-Json
        $release = switch ($Channel) {
            'stable' { $releases | Where-Object { -not $_.prerelease -and -not $_.draft } | Select-Object -First 1 }
            'beta'   { $releases | Where-Object { $_.prerelease -and -not $_.draft } | Select-Object -First 1 }
            'nightly' { $releases | Where-Object { $_.tag_name -like '*nightly*' -and -not $_.draft } | Select-Object -First 1 }
            default  { Fail "Unknown channel: $Channel" }
        }
        if (-not $release) {
            Fail "No release found for channel '$Channel'"
        }
        return $release.tag_name
    } catch {
        Fail "Failed to resolve latest version: $_"
    }
}

function Test-AdminRights {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Install-ViaWinget {
    param(
        [string]$Version = 'latest',
        [switch]$Preview,
        [switch]$Force
    )
    Write-Log "Checking for winget..."
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        Fail "winget not found. Please install Windows Package Manager (App Installer) from the Microsoft Store."
    }

    $pkgId = 'Astrivya.memnoth'
    $args = @('install', '--id', $pkgId, '--source', 'winget', '--accept-source-agreements', '--accept-package-agreements')

    if ($Version -ne 'latest') {
        $args += '--version', $Version.TrimStart('v')
    }

    if ($Force) {
        $args += '--force'
    }

    if ($Preview) {
        Write-Log "[Preview] Would run: winget $($args -join ' ')"
        return
    }

    Write-Log "Running: winget $($args -join ' ')"
    try {
        $result = & winget @args
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            Fail "winget install failed with exit code $exitCode"
        }
        Write-Log "winget installation complete!"
        Write-Log "Run 'memnoth --help' or 'mn --help' to get started"
    } catch {
        Fail "winget execution failed: $_"
    }
}

# ------------------------------------------------------------ main
Write-Log "memnoth installer starting..."

# Winget installation path
if ($UseWinget -or $Channel -eq 'winget') {
    Write-Log "Installing via winget..."
    Install-ViaWinget -Version $Version -Preview $Preview -Force $Force
    exit 0
}

# Resolve version
$resolvedVersion = Resolve-Version -Channel $Channel -Version $Version -BaseUrl $BaseUrl
Write-Log "Resolved version: $resolvedVersion"

# Platform detection (Windows x64 only for now)
$target = 'x86_64-pc-windows-msvc'
$artifact = "memnoth-$target.zip"
$archiveUrl = "$BaseUrl/$resolvedVersion/$artifact"
$checksumUrl = "$archiveUrl.sha256"

# Working directory
$workDir = Join-Path $env:TEMP "memnoth-install-$(Get-Random)"
if (Test-Path $workDir) { Remove-Item -Recurse -Force $workDir }
New-Item -ItemType Directory -Path $workDir | Out-Null

# Cleanup on exit
try {
    # -------------------------------------------------------- download archive
    $archivePath = Join-Path $workDir $artifact
    $status = Invoke-Download -Url $archiveUrl -OutFile $archivePath -Proxy $Proxy
    if ($status -eq 2) {
        Fail "Release not found: $archiveUrl`nCheck version/channel: $resolvedVersion / $Channel"
    }

    # -------------------------------------------------------- verify checksum
    if (-not $NoVerify) {
        $checksumPath = Join-Path $workDir "$artifact.sha256"
        $csStatus = Invoke-Download -Url $checksumUrl -OutFile $checksumPath -Proxy $Proxy
        if ($csStatus -ne 0 -or -not (Test-Path $checksumPath) -or (Get-Item $checksumPath).Length -eq 0) {
            Fail "Checksum not found at $checksumUrl; refusing to install unverified binary. Use -NoVerify to skip."
        }
        $expected = (Get-Content $checksumPath -Raw).Split(' ')[0].ToLowerInvariant()
        if (-not $expected) { Fail "Could not read expected checksum" }
        $actual = Get-Sha256 $archivePath
        if ($expected -ne $actual) {
            Fail "Checksum mismatch!`nExpected: $expected`nActual:   $actual"
        }
        Write-Log "Checksum verified OK"
    } else {
        Write-Log "WARNING: Skipping checksum verification (-NoVerify)" 'Warn'
    }

    # -------------------------------------------------------- extract
    $extractDir = Join-Path $workDir 'unpack'
    New-Item -ItemType Directory -Path $extractDir | Out-Null
    Write-Log "Extracting $artifact..."
    Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force

    # Find memnoth.exe
    $binary = Get-ChildItem -Path $extractDir -Filter 'memnoth.exe' -Recurse | Select-Object -First 1
    if (-not $binary) {
        Fail "Archive does not contain memnoth.exe"
    }

    # -------------------------------------------------------- install
    $installDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)
    Write-Log "Installing to $installDir"

    if (-not $Preview) {
        if (Test-Path $installDir -and -not $Force) {
            $confirm = Read-Host "Directory $installDir exists. Overwrite? [y/N]"
            if ($confirm -notin 'y', 'Y') { Fail "Installation cancelled" }
        }
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        Copy-Item -Path $binary.FullName -Destination (Join-Path $installDir 'memnoth.exe') -Force
        Write-Log "Installed memnoth.exe"
    } else {
        Write-Log "[Preview] Would install memnoth.exe to $installDir"
    }

    # -------------------------------------------------------- create mn alias
    if (-not $NoAlias) {
        $aliasPath = Join-Path $installDir 'mn.ps1'
        $aliasContent = @"
<#
.SYNOPSIS
    mn - memnoth short alias

.DESCRIPTION
    This is a generated alias script that forwards all arguments to memnoth.exe.
    It is created by the memnoth installer and updated on each install.
#>
`$memnoth = Join-Path `$PSScriptRoot 'memnoth.exe'
if (-not (Test-Path `$memnoth)) {
    Write-Error "memnoth.exe not found at `$memnoth"
    exit 1
}
& `$memnoth @args
"@
        if (-not $Preview) {
            Set-Content -Path $aliasPath -Value $aliasContent -Encoding UTF8
            Write-Log "Created mn.ps1 alias"
        } else {
            Write-Log "[Preview] Would create mn.ps1 alias at $aliasPath"
        }
    }

    # -------------------------------------------------------- add to user PATH
    if (-not $NoPath) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $installDirNormalized = $installDir.TrimEnd('\')
        if ($userPath -split ';' -contains $installDirNormalized) {
            Write-Log "$installDir already in user PATH"
        } else {
            if (-not $Preview) {
                $newPath = if ($userPath) { "$userPath;$installDirNormalized" } else { $installDirNormalized }
                [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
                Write-Log "Added $installDir to user PATH"
                Write-Log "NOTE: Restart your shell or run 'refreshenv' for PATH changes to take effect"
            } else {
                Write-Log "[Preview] Would add $installDir to user PATH"
            }
        }
    }

    # -------------------------------------------------------- verify installation
    if (-not $Preview) {
        $installedBinary = Join-Path $installDir 'memnoth.exe'
        if (Test-Path $installedBinary) {
            Write-Log "Verifying installation..."
            & $installedBinary --version
            if (-not $NoAlias) {
                $aliasPath = Join-Path $installDir 'mn.ps1'
                if (Test-Path $aliasPath) {
                    & $aliasPath --version > $null
                    Write-Log "Alias 'mn' verified"
                }
            }
            Write-Log "Installation complete!" 'Info'
            Write-Log "Run 'memnoth --help' or 'mn --help' to get started"
        } else {
            Fail "Installation verification failed: binary not found at $installedBinary"
        }
    }
}
finally {
    # Cleanup
    if (Test-Path $workDir) {
        Remove-Item -Recurse -Force $workDir -ErrorAction SilentlyContinue
    }
}

# ------------------------------------------------------------ winget/msi stubs (future)
<#
# WINGET STUB (for future use)
# ------------------------------------------------------------
# To publish to winget:
# 1. Create a manifest in the winget-pkgs repository
# 2. Manifest example:
#
# PackageIdentifier: Astrivya.memnoth
# PackageVersion: 0.1.0
# InstallerType: zip
# Installers:
#   - Architecture: x64
#     InstallerUrl: https://github.com/astrivya/memnoth/releases/download/v0.1.0/memnoth-x86_64-pc-windows-msvc.zip
#     InstallerSha256: <sha256>
#     Commands:
#       - memnoth
#       - mn
#
# MSI STUB (for future use)
# ------------------------------------------------------------
# To create an MSI installer:
# 1. Use WiX Toolset or similar
# 2. Include memnoth.exe
# 3. Install to Program Files\memnoth
# 4. Add to system PATH (requires admin)
# 5. Create Start Menu shortcuts
# 6. Register uninstaller
#
# Example WiX fragment:
# <Component Id="memnoth.exe" Guid="*">
#   <File Source="memnoth.exe" KeyPath="yes" />
# </Component>
# <Environment Id="PATH" Name="PATH" Value="[INSTALLDIR]" Action="set" Part="last" Permanent="yes" System="yes" />
#>

exit 0