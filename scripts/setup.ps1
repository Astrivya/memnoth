#requires -Version 5.1
<#
.SYNOPSIS
    /setup.ps1 — fresh-machine setup entry point (thin wrapper).

.DESCRIPTION
    Downloads the published installer, runs it with --setup, then prints the
    two CLI commands that finish the job. This wrapper contains no agent
    detection and writes no config: `mn setup --wizard` owns host detection
    and config files, and install.ps1 owns version resolution and archive
    verification. A forked installer is how a user runs a binary the releases
    never built.

    Trust boundary: this downloads and runs a script from memnoth.astrivya.ai
    over TLS. There is no script signature. The installer verifies the release
    archive by SHA-256 before installing it. The published 0.1.1 release is
    unsigned, so `mn update` refuses to self-update it.

    No secrets go in the URL: auth is a device flow and tokens live in the OS
    keyring, never in a shareable link. No arguments are forwarded, so a pasted
    command cannot smuggle `--token <TOKEN>` through to the CLI.

    MEMNOTH_INSTALL_BASE overrides the installer origin for a staging host or
    dist tree (the same variable install.ps1 reads).
#>
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if ($env:MEMNOTH_INSTALL_BASE) {
    $base = $env:MEMNOTH_INSTALL_BASE.TrimEnd('/')
    if ($base -like '*/releases/download') {
        # GitHub's `latest` is a redirect, not a tag; install.ps1 resolves it
        # the same way before asking for a release asset.
        $prefix = $base.Substring(0, $base.Length - '/releases/download'.Length)
        $installerUrl = "$prefix/releases/latest/download/install.ps1"
    } else {
        $installerUrl = "$base/install.ps1"
    }
} else {
    # The canonical site route serves the same published install.ps1 bytes.
    $installerUrl = 'https://memnoth.astrivya.ai/install.ps1'
}

Write-Host 'memnoth setup: note: this downloads and runs a script from the installer origin (memnoth.astrivya.ai by default); there is no script signature.'
Write-Host 'memnoth setup: note: the release archive is verified by SHA-256 before install.'
Write-Host 'memnoth setup: note: published 0.1.1 is unsigned, so `mn update` refuses self-update.'
Write-Host "memnoth setup: downloading installer from $installerUrl"

try {
    $installer = irm $installerUrl
    if (-not $installer) { throw "no installer text at $installerUrl" }
    # Piping straight to iex supplies no arguments. The scriptblock form is the
    # documented install.ps1 pattern for forwarding --setup: PowerShell 5.1
    # binds a double-dash token as a positional argument, which the installer's
    # catch-all parameter accepts.
    & ([scriptblock]::Create([string]$installer)) --setup
} catch {
    Write-Error "memnoth setup: failed to download or run the installer at $installerUrl : $($_.Exception.Message)"
    exit 1
}

Write-Host ''
Write-Host 'memnoth setup: install finished. Wire your agents and verify:'
Write-Host '  mn setup --wizard   # wire the agents detected on this machine'
Write-Host '  mn mcp doctor       # verify the MCP server'
