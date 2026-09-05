$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'This launcher is for Windows only.'
}

function Get-SpecialFolderPath([Environment+SpecialFolder]$Folder) {
    $Path = [Environment]::GetFolderPath($Folder)
    if ($Path) { return $Path }
    return $null
}

$LocalAppData = Get-SpecialFolderPath ([Environment+SpecialFolder]::LocalApplicationData)
if (-not $LocalAppData) { $LocalAppData = $env:LOCALAPPDATA }
if (-not $LocalAppData) { throw 'Unable to resolve the current user LocalApplicationData directory.' }

$Port = 9222
$Profile = if ($env:MCP_BROWSER_PROFILE_DIR) {
    $env:MCP_BROWSER_PROFILE_DIR
} else {
    Join-Path $LocalAppData 'mcp-bridge\chrome-profile'
}
$Profile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Profile)
[System.IO.Directory]::CreateDirectory($Profile) | Out-Null

$Candidates = New-Object System.Collections.Generic.List[string]
$ProgramFiles = Get-SpecialFolderPath ([Environment+SpecialFolder]::ProgramFiles)
$ProgramFilesX86 = Get-SpecialFolderPath ([Environment+SpecialFolder]::ProgramFilesX86)

foreach ($Base in @($ProgramFiles, $ProgramFilesX86, $LocalAppData)) {
    if (-not $Base) { continue }
    foreach ($Relative in @(
        'Google\Chrome\Application\chrome.exe',
        'Microsoft\Edge\Application\msedge.exe'
    )) {
        $Candidate = Join-Path $Base $Relative
        if (Test-Path -LiteralPath $Candidate) { $Candidates.Add($Candidate) }
    }
}

$Browser = $Candidates | Select-Object -First 1
if (-not $Browser) { throw 'No supported Chrome/Edge installation was found.' }

# Start-Process joins argument strings without quoting them. Double trailing
# backslashes before the closing quote so a profile ending in \ stays intact.
$QuotedProfile = '"' + ($Profile -replace '(\\+)$', '$1$1') + '"'
$Arguments = @(
    '--remote-debugging-address=127.0.0.1',
    "--remote-debugging-port=$Port",
    "--user-data-dir=$QuotedProfile"
)
Start-Process -FilePath $Browser -ArgumentList $Arguments | Out-Null
Write-Output "Started $Browser with CDP on 127.0.0.1:$Port"
