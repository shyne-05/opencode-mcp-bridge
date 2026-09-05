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

function Get-LocalAppDataPath {
    $Path = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    if (-not $Path) { $Path = $env:LOCALAPPDATA }
    if (-not $Path) { throw 'Unable to resolve the current user LocalApplicationData directory.' }
    return $Path
}

$Username = if ($env:MCP_OAUTH_USERNAME) { $env:MCP_OAUTH_USERNAME } else { 'admin' }
$EnvFile = if ($env:MCP_BRIDGE_ENV_FILE) {
    $env:MCP_BRIDGE_ENV_FILE
} else {
    Join-Path (Get-LocalAppDataPath) 'mcp-bridge\env'
}

$EnvFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($EnvFile)

function Read-EnvValue([string]$Name) {
    if (-not (Test-Path -LiteralPath $EnvFile)) { return $null }
    $prefix = "$Name="
    $match = Get-Content -LiteralPath $EnvFile -Encoding UTF8 | Where-Object { $_.StartsWith($prefix) } | Select-Object -Last 1
    if (-not $match) { return $null }
    return $match.Substring($prefix.Length)
}

if ($Show) {
    if (-not (Test-Path -LiteralPath $EnvFile)) { Fail "OAuth environment file does not exist: $EnvFile" }
    $StoredUser = Read-EnvValue 'MCP_OAUTH_USERNAME'
    $StoredPassword = Read-EnvValue 'MCP_OAUTH_PASSWORD'
    if ([string]::IsNullOrWhiteSpace($StoredUser) -or [string]::IsNullOrWhiteSpace($StoredPassword)) { Fail "OAuth credentials are incomplete in $EnvFile" }
    Write-Output "Username: $StoredUser"
    Write-Output "Password: $StoredPassword"
    exit 0
}

if (-not $PublicOrigin) { $PublicOrigin = $env:MCP_PUBLIC_URL }
if (-not $PublicOrigin) { Fail 'A public OAuth origin is required.' 2 }

if ([string]::IsNullOrWhiteSpace($Username) -or $Username -match '[\x00-\x1F\x7F]') {
    Fail 'OAuth username must be nonempty and contain no control characters.' 2
}

$ParsedOrigin = $null
if ($PublicOrigin -match '[\x00-\x20\x7F\\]' -or
    -not [Uri]::TryCreate($PublicOrigin, [UriKind]::Absolute, [ref]$ParsedOrigin)) {
    Fail 'Use an HTTPS origin without credentials, a path, query, or fragment, or loopback HTTP for local testing.' 2
}
$OriginHost = $ParsedOrigin.DnsSafeHost.TrimStart('[').TrimEnd(']')
# .NET Framework can expand DnsSafeHost to 0:0:0:0:0:0:0:1.
# Compare parsed addresses, retaining the bridge's exact loopback allowlist.
$OriginAddress = $null
$OriginIsLoopback = $OriginHost -ieq 'localhost'
if ([Net.IPAddress]::TryParse($OriginHost, [ref]$OriginAddress)) {
    $OriginIsLoopback = $OriginAddress.Equals([Net.IPAddress]::Loopback) -or
        $OriginAddress.Equals([Net.IPAddress]::IPv6Loopback)
}
if (($ParsedOrigin.Scheme -ne 'https' -and -not ($ParsedOrigin.Scheme -eq 'http' -and $OriginIsLoopback)) -or
    -not $ParsedOrigin.Host -or $ParsedOrigin.UserInfo -or $ParsedOrigin.Query -or $ParsedOrigin.Fragment -or
    ($ParsedOrigin.AbsolutePath -ne '/' -and $ParsedOrigin.AbsolutePath -ne '')) {
    Fail 'Use an HTTPS origin without credentials, a path, query, or fragment, or loopback HTTP for local testing.' 2
}
$PublicOrigin = $ParsedOrigin.GetLeftPart([UriPartial]::Authority)

$EnvDir = Split-Path -Parent $EnvFile
[System.IO.Directory]::CreateDirectory($EnvDir) | Out-Null

$ExistingPassword = if (-not $Rotate) { Read-EnvValue 'MCP_OAUTH_PASSWORD' } else { $null }
if (-not [string]::IsNullOrWhiteSpace($ExistingPassword)) {
    $Password = $ExistingPassword
} else {
    $Bytes = New-Object byte[] 24
    $Rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $Rng.GetBytes($Bytes) } finally { $Rng.Dispose() }
    $Password = -join ($Bytes | ForEach-Object { $_.ToString('x2') })
    if ($Password.Length -lt 24) { Fail 'Failed to generate a sufficiently strong OAuth password.' }
}

$Preserved = @()
if (Test-Path -LiteralPath $EnvFile) {
    $Preserved = Get-Content -LiteralPath $EnvFile -Encoding UTF8 | Where-Object {
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
    Move-Item -LiteralPath $TempFile -Destination $EnvFile -Force
}
finally {
    Remove-Item -LiteralPath $TempFile -Force -ErrorAction SilentlyContinue
}

# Windows ACLs, rather than Unix mode bits, protect this per-user file. The
# resolved LocalApplicationData directory follows the current Windows profile,
# including redirected profiles and non-C: system layouts.
Write-Output 'OAuth bootstrap configured safely.'
Write-Output "Username: $Username"
Write-Output "Credential file: $EnvFile"
Write-Output 'The password is stored locally and is not printed by default.'
Write-Output 'To view it on this machine: .\scripts\bootstrap-oauth.ps1 -Show'
if ($Rotate) { Write-Output 'Password rotated. Restart the bridge before reconnecting OAuth clients.' }
