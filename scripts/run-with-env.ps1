param(
    [string]$EnvFile = $env:MCP_BRIDGE_ENV_FILE
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $EnvFile) {
    $EnvFile = Join-Path $env:LOCALAPPDATA 'mcp-bridge/env'
}

if (Test-Path $EnvFile) {
    foreach ($line in Get-Content $EnvFile) {
        if ([string]::IsNullOrWhiteSpace($line) -or $line.TrimStart().StartsWith('#')) { continue }
        $index = $line.IndexOf('=')
        if ($index -le 0) { throw "invalid environment entry in $EnvFile" }
        $name = $line.Substring(0, $index)
        $value = $line.Substring($index + 1)
        if ($name -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { throw "invalid environment variable name in ${EnvFile}: $name" }
        [Environment]::SetEnvironmentVariable($name, $value, 'Process')
    }
}

$Binary = Join-Path $Root 'target/release/mcp-bridge.exe'
if (-not (Test-Path $Binary)) { throw "bridge binary not found: $Binary" }
Set-Location $Root
& $Binary
exit $LASTEXITCODE
