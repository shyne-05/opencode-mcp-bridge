$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Push-Location $Root
try {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    $TargetDir = Join-Path $Root 'target/release'
    New-Item -ItemType Directory -Path $TargetDir -Force | Out-Null
    Copy-Item (Join-Path $Root 'scripts/browser.cjs') (Join-Path $TargetDir 'browser.cjs') -Force

    node --check (Join-Path $TargetDir 'browser.cjs')
    if ($LASTEXITCODE -ne 0) { throw 'browser helper syntax check failed' }

    $OldNodePath = $env:NODE_PATH
    Remove-Item Env:NODE_PATH -ErrorAction SilentlyContinue
    try {
        $Protocol = (& node (Join-Path $TargetDir 'browser.cjs') version).Trim()
    }
    finally {
        if ($null -ne $OldNodePath) { $env:NODE_PATH = $OldNodePath }
    }
    if ($Protocol -ne 'mcp-browser-helper/2') {
        throw "unexpected browser helper protocol: $Protocol"
    }

    $VersionLine = Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern '^version = "([^"]+)"' | Select-Object -First 1
    if (-not $VersionLine) { throw 'could not determine package version' }
    $Version = $VersionLine.Matches[0].Groups[1].Value
    Write-Output "Packaged MCP Bridge $Version with browser helper protocol mcp-browser-helper/2"
}
finally {
    Pop-Location
}
