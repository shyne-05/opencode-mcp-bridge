param(
    [string]$EnvFile = $env:MCP_BRIDGE_ENV_FILE
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-LocalAppDataPath {
    $Path = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    if (-not $Path) { $Path = $env:LOCALAPPDATA }
    if (-not $Path) { throw 'Unable to resolve the current user LocalApplicationData directory.' }
    return $Path
}

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $EnvFile) {
    $EnvFile = Join-Path (Get-LocalAppDataPath) 'mcp-bridge\env'
}

if (-not $env:HOME -and $env:USERPROFILE) {
    $env:HOME = $env:USERPROFILE
}

if (Test-Path -LiteralPath $EnvFile) {
    foreach ($line in Get-Content -LiteralPath $EnvFile -Encoding UTF8) {
        if ([string]::IsNullOrWhiteSpace($line) -or $line.TrimStart().StartsWith('#')) { continue }
        $index = $line.IndexOf('=')
        if ($index -le 0) { throw "invalid environment entry in $EnvFile" }
        $name = $line.Substring(0, $index)
        $value = $line.Substring($index + 1)
        if ($name -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { throw "invalid environment variable name in $EnvFile" }
        [Environment]::SetEnvironmentVariable($name, $value, 'Process')
    }
}

$Binary = Join-Path $Root 'target\release\mcp-bridge.exe'
if (-not (Test-Path -LiteralPath $Binary)) { throw "bridge binary not found: $Binary" }
Set-Location -LiteralPath $Root
& $Binary
exit $LASTEXITCODE
