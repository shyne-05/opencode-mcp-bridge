param(
    [string]$TaskName = 'MCP Bridge'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'This installer is for Windows only.'
}

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Push-Location $Root
try {
    $ExistingTask = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($ExistingTask) {
        Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
        for ($i = 0; $i -lt 40; $i++) {
            $State = (Get-ScheduledTask -TaskName $TaskName).State
            if ($State -ne 'Running') { break }
            Start-Sleep -Milliseconds 250
        }
        if ((Get-ScheduledTask -TaskName $TaskName).State -eq 'Running') {
            throw "scheduled task did not stop: $TaskName"
        }
    }

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
    Write-Output "Installed/updated and started Windows scheduled task: $TaskName"
}
finally {
    Pop-Location
}
