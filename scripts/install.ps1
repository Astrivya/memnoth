<#
.SYNOPSIS
    memnoth installer for Windows (PowerShell 5.1+)

.DESCRIPTION
    Downloads, verifies, and installs memnoth CLI from GitHub releases.
    Installs to %LOCALAPPDATA%\memnoth\bin, adds to user PATH, creates 'mn' alias.

    If an asset cannot be fetched from -BaseUrl, the same release path is retried
    once against https://github.com/astrivya/memnoth/releases/download. The
    retry is always announced and the archive is still SHA-256 verified.

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

.PARAMETER Setup
    After installing, run `mn setup --yes` once. Off by default. A failed setup
    is a warning with its last lines, not a failed install; rerun with `mn setup`.
    Also accepted from the one-liner as --setup (and -setup, --with-setup).

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

.EXAMPLE
    # Install and run first-run setup (named switch)
    .\install.ps1 -Setup

.EXAMPLE
    # Install and run first-run setup from the one-line download form
    & ([scriptblock]::Create((irm https://memnoth.astrivya.ai/install.ps1))) --setup
#>

[CmdletBinding()]
param(
    # Windows PowerShell 5.1 binds a double-dash token such as --setup as a
    # POSITIONAL argument, not as a named switch, so a one-liner that forwards
    # --setup would otherwise fail validation on $Channel before the body runs.
    # Capture leftover positional tokens here; the setup detector below reads
    # them. All named parameters still bind normally.
    [Parameter(Position = 0, ValueFromRemainingArguments = $true)]
    [string[]]$RawArgs,

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

    [switch]$UseWinget,

    [Alias('WithSetup', 'with-setup')]
    [switch]$Setup
)

# Enable strict mode for better error handling
Set-StrictMode -Version Latest

$ErrorActionPreference = 'Stop'

# Progress preference for cleaner output
$ProgressPreference = 'SilentlyContinue'

# A trailing slash would otherwise produce `//v0.1.1/…` in every URL.
$BaseUrl = $BaseUrl.TrimEnd('/')

# Last-resort source. Kept separate from BaseUrl so a site mirror or staging
# host is always tried first and GitHub is used only when that source fails.
$CanonicalBase = 'https://github.com/astrivya/memnoth/releases/download'

# --setup is opt-in. Accept the declared switch (-Setup) and the literal tokens
# a one-liner can forward, whether they arrive as named parameters, as raw
# positional args (--setup), or in $args. Named parameters and argument
# forwarding are otherwise untouched.
$setupTokens = @('--setup', '-setup', '-Setup', '--with-setup', '-with-setup')
$doSetup = [bool]$Setup
if (-not $doSetup) {
    foreach ($token in @($RawArgs) + @($args)) {
        if ($setupTokens -contains [string]$token) {
            $doSetup = $true
            break
        }
    }
}
$setupOk = $false

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
        [string]$Proxy,
        [switch]$Quiet
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
        # On filesystem errors (for example, a full TEMP volume) there is no
        # HTTP Response. Read it through PSObject.Properties so strict mode
        # preserves the useful underlying exception instead of masking it.
        $responseProperty = $_.Exception.PSObject.Properties['Response']
        if ($responseProperty -and $responseProperty.Value) {
            $statusProperty = $responseProperty.Value.StatusCode.PSObject.Properties['value__']
            if ($statusProperty) {
                $status = [int]$statusProperty.Value
            } else {
                $status = [int]$responseProperty.Value.StatusCode
            }
        }
        if ($status -eq 404) { return 2 }
        if ($Quiet) {
            Write-Log "Primary source failed for $Url : $_" 'Warn'
            return 1
        }
        Fail "Download failed for $Url : $_"
    }
    return 0
}

# Try the configured source first; only on failure retry the same release path
# against the canonical GitHub release base. The switch is never silent, and
# checksum verification runs over whatever bytes land in OutFile, so the
# fallback download is trusted no more than the primary one.
function Invoke-DownloadWithFallback {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][string]$OutFile,
        [string]$Proxy,
        [string]$FallbackUrl
    )
    $status = Invoke-Download -Url $Url -OutFile $OutFile -Proxy $Proxy -Quiet
    if ($status -eq 0 -or [string]::IsNullOrEmpty($FallbackUrl) -or $FallbackUrl -eq $Url) {
        return $status
    }
    Write-Log "Primary source failed; retrying from $FallbackUrl" 'Warn'
    $fallbackStatus = Invoke-Download -Url $FallbackUrl -OutFile $OutFile -Proxy $Proxy -Quiet
    if ($fallbackStatus -ne 0) {
        Write-Log "Fallback source also failed for $OutFile (status $fallbackStatus)" 'Error'
    }
    return $fallbackStatus
}

function Get-InstallerWorkDirectory {
    $candidates = @()
    if ($env:MEMNOTH_TEMP_DIR) { $candidates += $env:MEMNOTH_TEMP_DIR }
    if ($env:TEMP) { $candidates += $env:TEMP }
    if ($env:LOCALAPPDATA) { $candidates += (Join-Path $env:LOCALAPPDATA 'Temp') }
    $systemTemp = [System.IO.Path]::GetTempPath()
    if ($systemTemp) { $candidates += $systemTemp }
    if ($env:USERPROFILE) { $candidates += (Join-Path $env:USERPROFILE 'AppData\Local\Temp') }

    $seen = @{}
    foreach ($candidate in $candidates) {
        if ([string]::IsNullOrWhiteSpace($candidate)) { continue }
        try {
            $fullCandidate = [System.IO.Path]::GetFullPath($candidate)
            $key = $fullCandidate.TrimEnd('\').ToLowerInvariant()
            if ($seen.ContainsKey($key)) { continue }
            $seen[$key] = $true
            if (-not (Test-Path -LiteralPath $fullCandidate -PathType Container)) {
                New-Item -ItemType Directory -Path $fullCandidate -Force -ErrorAction Stop | Out-Null
            }

            $root = [System.IO.Path]::GetPathRoot($fullCandidate)
            if ($root) {
                $drive = [System.IO.DriveInfo]::new($root)
                if ($drive.IsReady -and $drive.AvailableFreeSpace -lt 134217728) {
                    Write-Log "Skipping temporary volume with less than 128 MiB free: $fullCandidate" 'Debug'
                    continue
                }
            }

            $workPath = Join-Path $fullCandidate ("memnoth-install-" + [Guid]::NewGuid().ToString('N'))
            New-Item -ItemType Directory -Path $workPath -ErrorAction Stop | Out-Null
            return $workPath
        } catch {
            Write-Log "Temporary directory unavailable ($candidate): $($_.Exception.Message)" 'Debug'
        }
    }
    throw 'No writable temporary directory with at least 128 MiB free. Set MEMNOTH_TEMP_DIR to a directory on a volume with enough space.'
}

function ConvertTo-ReleaseDir {
    param([string]$Version)
    # dist.sh names release directories with a leading `v`, while `mn version`
    # and the manifest's `version` field carry the bare semver. Accept both.
    if ($Version -eq 'latest' -or $Version.StartsWith('v')) { return $Version }
    return "v$Version"
}

function Resolve-Version {
    param([string]$Channel, [string]$Version, [string]$BaseUrl)
    if ($Version -ne 'latest') {
        return (ConvertTo-ReleaseDir $Version)
    }

    $normalizedBase = $BaseUrl.TrimEnd('/')

    # A dist tree states its own latest version in <base>/latest/manifest.json —
    # the same document `mn update` reads. Use it for every non-GitHub base: the
    # old code always called api.github.com, so `-BaseUrl <staging host>` still
    # resolved against GitHub and installed the wrong release.
    if ($normalizedBase -notlike '*/releases/download') {
        $manifestUrl = "$normalizedBase/latest/manifest.json"
        $fallbackManifestUrl = "$CanonicalBase/latest/manifest.json"
        Write-Log "Resolving latest version from $manifestUrl"
        try {
            $latestManifest = Invoke-RestMethod -Uri $manifestUrl -UseBasicParsing -ErrorAction Stop
            if ($latestManifest -and $latestManifest.version) {
                return (ConvertTo-ReleaseDir ([string]$latestManifest.version))
            }
        } catch {
            Write-Log "No usable manifest at $manifestUrl : $_" 'Warn'
        }
        if ($fallbackManifestUrl -ne $manifestUrl) {
            Write-Log "Retrying version resolution from $fallbackManifestUrl" 'Warn'
            try {
                $latestManifest = Invoke-RestMethod -Uri $fallbackManifestUrl -UseBasicParsing -ErrorAction Stop
                if ($latestManifest -and $latestManifest.version) {
                    return (ConvertTo-ReleaseDir ([string]$latestManifest.version))
                }
            } catch {
                Write-Log "No usable manifest at $fallbackManifestUrl : $_" 'Warn'
            }
        }
        Fail "Cannot resolve 'latest' at ${normalizedBase}: no manifest.json there"
    }

    # GitHub has no `latest` tag; its API is the only place that names one.
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
    # Do not overwrite the automatic $args variable; use a named argument list.
    $wingetArgs = @('install', '--id', $pkgId, '--source', 'winget', '--accept-source-agreements', '--accept-package-agreements')

    if ($Version -ne 'latest') {
        $wingetArgs += '--version', $Version.TrimStart('v')
    }

    if ($Force) {
        $wingetArgs += '--force'
    }

    if ($Preview) {
        Write-Log "[Preview] Would run: winget $($wingetArgs -join ' ')"
        return
    }

    Write-Log "Running: winget $($wingetArgs -join ' ')"
    try {
        $result = & winget @wingetArgs
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
$expectedFromManifest = $null
$releaseManifest = $null
$manifestUrl = "$BaseUrl/$resolvedVersion/manifest.json"
$fallbackManifestUrl = "$CanonicalBase/$resolvedVersion/manifest.json"
try {
    $releaseManifest = Invoke-RestMethod -Uri $manifestUrl -UseBasicParsing -ErrorAction Stop
} catch {
    Write-Log "No manifest at $manifestUrl" 'Debug'
    if ($fallbackManifestUrl -ne $manifestUrl) {
        Write-Log "Retrying manifest from $fallbackManifestUrl" 'Warn'
        try {
            $releaseManifest = Invoke-RestMethod -Uri $fallbackManifestUrl -UseBasicParsing -ErrorAction Stop
        } catch {
            Write-Log "No manifest at $fallbackManifestUrl; using sidecar checksum" 'Debug'
        }
    }
}
if ($releaseManifest) {
    try {
        $manifestTarget = $releaseManifest.targets | Where-Object { $_.triple -eq $target } | Select-Object -First 1
        if ($manifestTarget -and $manifestTarget.filename) {
            $artifact = $manifestTarget.filename
            if ($manifestTarget.sha256) { $expectedFromManifest = [string]$manifestTarget.sha256 }
            Write-Log "Using manifest target $target ($artifact)"
        }
    } catch {
        Write-Log "Manifest had no usable target for $target; using sidecar checksum" 'Debug'
    }
}
if ($releaseManifest -and $releaseManifest.signatures -and -not $releaseManifest.signatures.minisign) {
    Write-Log "This release is unsigned; a script install verifies its SHA-256 digest, but 'mn update' will refuse it until a signature is published." 'Warn'
}
$archiveUrl = "$BaseUrl/$resolvedVersion/$artifact"
$checksumUrl = "$archiveUrl.sha256"
$fallbackArchiveUrl = "$CanonicalBase/$resolvedVersion/$artifact"
$fallbackChecksumUrl = "$fallbackArchiveUrl.sha256"

# Working directory. Prefer MEMNOTH_TEMP_DIR, then fall back across the normal
# Windows temp locations. A unique directory avoids deleting or reusing a
# pre-existing path, even if two installer processes start at the same time.
try {
    $workDir = Get-InstallerWorkDirectory
    Write-Log "Using temporary workspace $workDir" 'Debug'
} catch {
    Fail $_.Exception.Message
}

# Cleanup on exit
try {
    # -------------------------------------------------------- download archive
    $archivePath = Join-Path $workDir $artifact
    $status = Invoke-DownloadWithFallback -Url $archiveUrl -OutFile $archivePath -Proxy $Proxy -FallbackUrl $fallbackArchiveUrl
    if ($status -eq 2) {
        Fail "Release not found: $archiveUrl`nCheck version/channel: $resolvedVersion / $Channel"
    }

    # -------------------------------------------------------- verify checksum
    if (-not $NoVerify) {
        $expected = $null
        if ($expectedFromManifest) {
            $expected = $expectedFromManifest.ToLowerInvariant()
            Write-Log "Checksum from manifest"
        } else {
            $checksumPath = Join-Path $workDir "$artifact.sha256"
            $csStatus = Invoke-DownloadWithFallback -Url $checksumUrl -OutFile $checksumPath -Proxy $Proxy -FallbackUrl $fallbackChecksumUrl
            if ($csStatus -ne 0 -or -not (Test-Path $checksumPath) -or (Get-Item $checksumPath).Length -eq 0) {
                Fail "Checksum not found at $checksumUrl; refusing to install unverified binary. Use -NoVerify to skip."
            }
            # Trim first: a BOM or trailing newline in a sidecar must not become
            # part of the digest and turn a good download into a false mismatch.
            $expected = (Get-Content $checksumPath -Raw).Trim().Split(' ')[0].ToLowerInvariant()
        }
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
        if ((Test-Path $installDir) -and -not $Force) {
            $confirm = Read-Host "Directory $installDir exists. Overwrite? [y/N]"
            if ($confirm -notin 'y', 'Y') { Fail "Installation cancelled" }
        }
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        Copy-Item -Path $binary.FullName -Destination (Join-Path $installDir 'memnoth.exe') -Force
        Write-Log "Installed memnoth.exe"

        # The server travels with the CLI: `memnoth mcp serve` execs
        # memnoth-mcp.exe from its own directory. A server left in the unpack
        # dir is never found, and the CLI then falls back to whatever is on
        # PATH — which is how an older install answered with an older surface.
        # Older archives carry no server; that is not an install failure.
        $server = Get-ChildItem -Path $extractDir -Filter 'memnoth-mcp.exe' -Recurse | Select-Object -First 1
        if ($server) {
            Copy-Item -Path $server.FullName -Destination (Join-Path $installDir 'memnoth-mcp.exe') -Force
            Write-Log "Installed memnoth-mcp.exe"
        }
    } else {
        Write-Log "[Preview] Would install memnoth.exe to $installDir"
    }

    # -------------------------------------------------------- create mn alias
    if (-not $NoAlias) {
        $aliasPath = Join-Path $installDir 'mn.ps1'
        $aliasLines = @(
            '<#',
            '.SYNOPSIS',
            '    mn - memnoth short alias',
            '',
            '.DESCRIPTION',
            '    This is a generated alias script that forwards all arguments to memnoth.exe.',
            '    It is created by the memnoth installer and updated on each install.',
            '#>',
            '$memnoth = Join-Path $PSScriptRoot ''memnoth.exe''',
            'if (-not (Test-Path $memnoth)) {',
            '    Write-Error "memnoth.exe not found at $memnoth"',
            '    exit 1',
            '}',
            '& $memnoth @args',
            ''
        )
        $aliasContent = $aliasLines -join [Environment]::NewLine
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
            # -------------------------------------------------------- optional setup
            # Opt-in only. The binaries are installed by now, so a failed setup
            # stays a warning: report the captured last lines and point at the
            # recoverable `mn setup` command instead of failing the install.
            if ($doSetup) {
                Write-Log "Running setup --yes (opt-in)..."
                try {
                    $setupLines = @(& $installedBinary setup --yes *>&1 | ForEach-Object { $_.ToString() })
                    $setupExit = $LASTEXITCODE
                    if ($setupExit -eq 0) {
                        Write-Log "setup --yes completed"
                        $setupOk = $true
                    } else {
                        Write-Log "setup --yes exited $setupExit; binaries are installed and you can retry with 'mn setup'." 'Warn'
                        Write-Log "setup output (last lines):" 'Warn'
                        $setupLines | Select-Object -Last 15 | ForEach-Object { Write-Log $_ 'Warn' }
                        Write-Log "setup reason shown above; the install itself succeeded" 'Warn'
                    }
                } catch {
                    Write-Log "setup --yes failed: $($_.Exception.Message); binaries are installed and you can retry with 'mn setup'." 'Warn'
                }
            }

            Write-Log "Installation complete!" 'Info'

            # -------------------------------------------------------- next steps
            # Copyable commands only, no marketing prose. The MCP line is shown
            # only when the installed binary advertises both flags it needs.
            Write-Host ''
            Write-Log 'Next:' 'Info'
            Write-Host '  mn doctor'
            if ($setupOk) {
                Write-Host '  mn setup --yes  (already ran during install)'
            } else {
                Write-Host '  mn setup --yes'
            }
            $mcpHelp = ''
            try {
                $mcpHelp = (& $installedBinary mcp add --help 2>$null | Out-String -Width 4096)
            } catch {
                $mcpHelp = ''
            }
            if ($mcpHelp -match '--local' -and $mcpHelp -match '--clients') {
                Write-Host '  mn mcp add --local --clients claude,cursor,codex,zed'
            }
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
