param(
    [Parameter(Position = 0)]
    [string]$PublicOrigin,
    [switch]$Show,
    [switch]$Rotate
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail([string]$Message, [int]$Code = 1) {
    [Console]::Error.WriteLine($Message)
    exit $Code
}

$Username = if ($env:MCP_OAUTH_USERNAME) { $env:MCP_OAUTH_USERNAME } else { 'admin' }
$EnvFile = if ($env:MCP_BRIDGE_ENV_FILE) {
    $env:MCP_BRIDGE_ENV_FILE
} else {
    Join-Path $env:LOCALAPPDATA 'mcp-bridge/env'
}

function Read-EnvValue([string]$Name) {
    if (-not (Test-Path $EnvFile)) { return $null }
    $prefix = "$Name="
    $match = Get-Content $EnvFile | Where-Object { $_.StartsWith($prefix) } | Select-Object -Last 1
    if (-not $match) { return $null }
    return $match.Substring($prefix.Length)
}

if ($Show) {
    if (-not (Test-Path $EnvFile)) { Fail "OAuth environment file does not exist: $EnvFile" }
    $StoredUser = Read-EnvValue 'MCP_OAUTH_USERNAME'
    $StoredPassword = Read-EnvValue 'MCP_OAUTH_PASSWORD'
    if (-not $StoredUser -or -not $StoredPassword) { Fail "OAuth credentials are incomplete in $EnvFile" }
    Write-Output "Username: $StoredUser"
    Write-Output "Password: $StoredPassword"
    exit 0
}

if (-not $PublicOrigin) { $PublicOrigin = $env:MCP_PUBLIC_URL }
if (-not $PublicOrigin) { Fail 'A public OAuth origin is required.' 2 }

$HttpsOrigin = '^https://[^/]+(:[0-9]+)?$'
$LoopbackOrigin = '^http://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]+)?$'
if ($PublicOrigin -notmatch $HttpsOrigin -and $PublicOrigin -notmatch $LoopbackOrigin) {
    Fail "Invalid public origin: $PublicOrigin`nUse an HTTPS origin without a path, or loopback HTTP for local testing." 2
}

$EnvDir = Split-Path -Parent $EnvFile
New-Item -ItemType Directory -Path $EnvDir -Force | Out-Null

$ExistingPassword = if (-not $Rotate) { Read-EnvValue 'MCP_OAUTH_PASSWORD' } else { $null }
if ($ExistingPassword) {
    $Password = $ExistingPassword
} else {
    $Bytes = New-Object byte[] 24
    $Rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $Rng.GetBytes($Bytes) } finally { $Rng.Dispose() }
    $Password = -join ($Bytes | ForEach-Object { $_.ToString('x2') })
}
if ($Password.Length -lt 24) { Fail 'Failed to generate a sufficiently strong OAuth password.' }

$Preserved = @()
if (Test-Path $EnvFile) {
    $Preserved = Get-Content $EnvFile | Where-Object {
        $_ -notmatch '^(MCP_PUBLIC_URL|MCP_OAUTH_USERNAME|MCP_OAUTH_PASSWORD|MCP_OAUTH_ALLOW_INSECURE_HTTP)='
    }
}

$Lines = @($Preserved)
$Lines += "MCP_PUBLIC_URL=$PublicOrigin"
$Lines += "MCP_OAUTH_USERNAME=$Username"
$Lines += "MCP_OAUTH_PASSWORD=$Password"
if ($PublicOrigin.StartsWith('http://')) { $Lines += 'MCP_OAUTH_ALLOW_INSECURE_HTTP=true' }

$TempFile = "$EnvFile.tmp.$PID"
try {
    [System.IO.File]::WriteAllLines($TempFile, $Lines, (New-Object System.Text.UTF8Encoding($false)))
    Move-Item $TempFile $EnvFile -Force
}
finally {
    Remove-Item $TempFile -Force -ErrorAction SilentlyContinue
}

# Windows ACLs, rather than Unix mode bits, protect this per-user file. The
# default LOCALAPPDATA directory is private to the current user profile.
Write-Output 'OAuth bootstrap configured safely.'
Write-Output "Username: $Username"
Write-Output "Credential file: $EnvFile"
Write-Output 'The password is stored locally and is not printed by default.'
Write-Output 'To view it on this machine: .\scripts\bootstrap-oauth.ps1 -Show'
if ($Rotate) { Write-Output 'Password rotated. Restart the bridge before reconnecting OAuth clients.' }
