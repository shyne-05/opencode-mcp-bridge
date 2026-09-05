param(
    [string]$TaskName = 'MCP Bridge'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'This installer is for Windows only.'
}

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Runner = Join-Path $Root 'scripts/run-with-env.ps1'
$PowerShell = (Get-Command powershell.exe -CommandType Application -ErrorAction Stop).Source
foreach ($RequiredCommand in @('node', 'cargo')) {
    if (-not (Get-Command $RequiredCommand -CommandType Application -ErrorAction SilentlyContinue)) {
        throw "$RequiredCommand is required before installing or updating the scheduled task."
    }
}
$Arguments = '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $Runner + '"'
if ($env:MCP_BRIDGE_ENV_FILE) {
    $EnvFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($env:MCP_BRIDGE_ENV_FILE)
    if (-not (Test-Path -LiteralPath $EnvFile -PathType Leaf)) {
        throw 'The configured MCP_BRIDGE_ENV_FILE must be an existing file.'
    }
    $Arguments += ' -EnvFile "' + $EnvFile + '"'
}
Push-Location -LiteralPath $Root
$ResumeExistingTask = $false
try {
    $ExistingTask = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($ExistingTask -and $ExistingTask.State -eq 'Running') {
        $ResumeExistingTask = $true
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

    $Action = New-ScheduledTaskAction -Execute $PowerShell -Argument $Arguments -WorkingDirectory $Root
    $Trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
    $Settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero)
    Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Trigger -Settings $Settings -Description 'MCP Bridge personal desktop service' -Force | Out-Null
    Start-ScheduledTask -TaskName $TaskName
    $ResumeExistingTask = $false
    Write-Output "Installed/updated and started Windows scheduled task: $TaskName"
}
catch {
    $InstallError = $_
    if ($ResumeExistingTask) {
        try {
            Start-ScheduledTask -TaskName $TaskName -ErrorAction Stop
        }
        catch {
            Write-Warning "Could not resume the previously running scheduled task: $TaskName"
        }
    }
    throw $InstallError
}
finally {
    Pop-Location
}
