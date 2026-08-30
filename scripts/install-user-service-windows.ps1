param(
    [string]$TaskName = 'MCP Bridge'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows) { throw 'This installer is for Windows only.' }

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Push-Location $Root
try {
    & (Join-Path $Root 'scripts/package-release.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'release packaging failed' }

    $Runner = Join-Path $Root 'scripts/run-with-env.ps1'
    $PowerShell = (Get-Command powershell.exe).Source
    $Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$Runner`""
    $Action = New-ScheduledTaskAction -Execute $PowerShell -Argument $Arguments -WorkingDirectory $Root
    $Trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
    $Settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero)
    Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Trigger -Settings $Settings -Description 'MCP Bridge personal desktop service' -Force | Out-Null
    Start-ScheduledTask -TaskName $TaskName
    Write-Output "Installed and started Windows scheduled task: $TaskName"
}
finally {
    Pop-Location
}
