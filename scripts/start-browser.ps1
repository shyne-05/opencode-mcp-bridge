param(
    [int]$Port = 9222
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows) { throw 'This launcher is for Windows only.' }

$Profile = if ($env:MCP_BROWSER_PROFILE_DIR) {
    $env:MCP_BROWSER_PROFILE_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'mcp-bridge\chrome-profile'
}
New-Item -ItemType Directory -Path $Profile -Force | Out-Null

$Candidates = @(
    (Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Google\Chrome\Application\chrome.exe'),
    (Join-Path $env:LOCALAPPDATA 'Google\Chrome\Application\chrome.exe'),
    (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe')
) | Where-Object { $_ -and (Test-Path $_) }

$Browser = $Candidates | Select-Object -First 1
if (-not $Browser) { throw 'No supported Chrome/Edge installation was found.' }

$Arguments = @(
    '--remote-debugging-address=127.0.0.1',
    "--remote-debugging-port=$Port",
    "--user-data-dir=$Profile"
)
Start-Process -FilePath $Browser -ArgumentList $Arguments | Out-Null
Write-Output "Started $Browser with CDP on 127.0.0.1:$Port"
