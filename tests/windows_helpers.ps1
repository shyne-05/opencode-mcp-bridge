$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'These fixtures require Windows PowerShell 5.1 or PowerShell 7 on Windows.'
}

$RepoRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$FixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('mcp-helpers [test] ' + [guid]::NewGuid())
$Project = Join-Path $FixtureRoot ('repo [one] ' + [char]0x00E9)
$Utf8 = New-Object System.Text.UTF8Encoding($false)
$HostExecutable = (Get-Process -Id $PID).Path
$FixtureState = [pscustomobject]@{
    BrowserArguments = @()
    HostExecutable = $HostExecutable
    TaskEvents = (New-Object 'System.Collections.Generic.List[string]')
    TaskState = 'Ready'
    PackageFails = $false
    MissingNode = $false
    TaskArguments = ''
    TaskDirectory = ''
}
$SavedEnvironment = @{}
foreach ($Name in @('MCP_BRIDGE_ENV_FILE', 'MCP_OAUTH_USERNAME', 'MCP_BROWSER_PROFILE_DIR', 'MCP_BRIDGE_TEST_CAPTURE')) {
    $SavedEnvironment[$Name] = [Environment]::GetEnvironmentVariable($Name, 'Process')
}

function Assert-Fixture([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Invoke-PowerShellFixture([string]$Script, [string]$Arguments) {
    $Info = New-Object Diagnostics.ProcessStartInfo
    $Info.FileName = $HostExecutable
    $Info.Arguments = '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $Script + '" ' + $Arguments
    $Info.WorkingDirectory = $FixtureRoot
    $Info.UseShellExecute = $false
    $Info.RedirectStandardOutput = $true
    $Info.RedirectStandardError = $true
    $Process = New-Object Diagnostics.Process
    $Process.StartInfo = $Info
    try {
        [void]$Process.Start()
        $Output = $Process.StandardOutput.ReadToEnd()
        $ErrorOutput = $Process.StandardError.ReadToEnd()
        $Process.WaitForExit()
        return [pscustomobject]@{ ExitCode = $Process.ExitCode; Output = $Output + $ErrorOutput }
    }
    finally {
        $Process.Dispose()
    }
}

try {
    [IO.Directory]::CreateDirectory((Join-Path $Project 'scripts')) | Out-Null
    [IO.Directory]::CreateDirectory((Join-Path $Project 'target\release')) | Out-Null

    # Replace only the executable suffix in a temporary runner copy. Its target
    # inspects synthetic values instead of starting the actual bridge.
    $Runner = Join-Path $Project 'scripts\run-with-env.ps1'
    $Source = [IO.File]::ReadAllText((Join-Path $RepoRoot 'scripts\run-with-env.ps1'))
    [IO.File]::WriteAllText($Runner, $Source.Replace('target\release\mcp-bridge.exe', 'target\release\mcp-bridge.ps1'), $Utf8)
    [IO.File]::WriteAllText((Join-Path $Project 'target\release\mcp-bridge.ps1'), @'
$Observed = @{
    unicode = $env:BRIDGE_TEST_UNICODE
    literal = $env:BRIDGE_TEST_LITERAL
    last = $env:BRIDGE_TEST_LAST
}
[IO.File]::WriteAllText($env:MCP_BRIDGE_TEST_CAPTURE, ($Observed | ConvertTo-Json -Compress), (New-Object Text.UTF8Encoding($false)))
$global:LASTEXITCODE = 0
'@, $Utf8)
    $EnvFile = Join-Path $FixtureRoot 'runner [settings].env'
    $env:MCP_BRIDGE_TEST_CAPTURE = Join-Path $FixtureRoot 'observed.json'
    $Unicode = 'caf' + [char]0x00E9 + ' ' + [char]0x4E2D
    $Literal = '"$(throw ''must not execute'') $env:HOME \path = value"'
    $Lines = @('# comment', '', "BRIDGE_TEST_UNICODE=$Unicode", "BRIDGE_TEST_LITERAL=$Literal", 'BRIDGE_TEST_LAST=final=value')
    [IO.File]::WriteAllText($EnvFile, ($Lines -join ([string][char]13 + [char]10)), $Utf8)
    $Result = Invoke-PowerShellFixture $Runner ('-EnvFile "' + $EnvFile + '"')
    Assert-Fixture ($Result.ExitCode -eq 0) ('runner failed: ' + $Result.Output)
    $Observed = [IO.File]::ReadAllText($env:MCP_BRIDGE_TEST_CAPTURE) | ConvertFrom-Json
    Assert-Fixture ($Observed.unicode -ceq $Unicode) 'runner changed UTF-8 data'
    Assert-Fixture ($Observed.literal -ceq $Literal) 'runner evaluated or changed literal data'
    Assert-Fixture ($Observed.last -ceq 'final=value') 'runner dropped the final unterminated assignment'

    [IO.File]::Delete($env:MCP_BRIDGE_TEST_CAPTURE)
    [IO.File]::WriteAllText($EnvFile, 'invalid-secret-canary=value', $Utf8)
    $Result = Invoke-PowerShellFixture $Runner ('-EnvFile "' + $EnvFile + '"')
    Assert-Fixture ($Result.ExitCode -ne 0) 'runner accepted an invalid environment name'
    Assert-Fixture (-not $Result.Output.Contains('secret-canary')) 'runner revealed malformed secret contents'
    Assert-Fixture (-not [IO.File]::Exists($env:MCP_BRIDGE_TEST_CAPTURE)) 'runner executed after invalid configuration'

    # Bootstrap must support relative bracket paths and preserve UTF-8 entries
    # when read again by Windows PowerShell 5.1, whose default is otherwise ANSI.
    Push-Location -LiteralPath $FixtureRoot
    try {
        $env:MCP_BRIDGE_ENV_FILE = '.\oauth [settings].env'
        $env:MCP_OAUTH_USERNAME = $Unicode
        $OAuthFile = Join-Path $FixtureRoot 'oauth [settings].env'
        [IO.File]::WriteAllText($OAuthFile, "CUSTOM=$Unicode", $Utf8)
        $Bootstrap = Join-Path $RepoRoot 'scripts\bootstrap-oauth.ps1'
        $FirstLog = & $Bootstrap 'http://127.0.0.1:3000/'
        $FirstLines = [IO.File]::ReadAllLines($OAuthFile)
        $PasswordLine = $FirstLines | Where-Object { $_.StartsWith('MCP_OAUTH_PASSWORD=') }
        $SecondLog = & $Bootstrap 'http://[::1]:3000'
        $SecondLines = [IO.File]::ReadAllLines($OAuthFile)
        Assert-Fixture ($SecondLines -ccontains "CUSTOM=$Unicode") 'bootstrap corrupted an existing UTF-8 entry'
        Assert-Fixture ($SecondLines -ccontains "MCP_OAUTH_USERNAME=$Unicode") 'bootstrap corrupted the username'
        Assert-Fixture ($SecondLines -ccontains $PasswordLine) 'bootstrap unexpectedly rotated the password'
        Assert-Fixture ($SecondLines -contains 'MCP_PUBLIC_URL=http://[::1]:3000') 'bootstrap changed the IPv6 origin'
        $Password = $PasswordLine.Substring('MCP_OAUTH_PASSWORD='.Length)
        Assert-Fixture ($Password.Length -ge 24) 'bootstrap generated a short password'
        Assert-Fixture (-not (($FirstLog + $SecondLog | Out-String).Contains($Password))) 'bootstrap printed its generated password'

        $RotateLog = & $Bootstrap -Rotate 'https://example.test'
        $RotatedLine = [IO.File]::ReadAllLines($OAuthFile) | Where-Object { $_.StartsWith('MCP_OAUTH_PASSWORD=') }
        $Rotated = $RotatedLine.Substring('MCP_OAUTH_PASSWORD='.Length)
        Assert-Fixture ($Rotated -cne $Password -and $Rotated.Length -ge 24) 'bootstrap failed to rotate its password'
        Assert-Fixture (-not (($RotateLog | Out-String).Contains($Rotated))) 'bootstrap printed the rotated password'

        $WhitespaceLines = [IO.File]::ReadAllLines($OAuthFile) | ForEach-Object {
            if ($_.StartsWith('MCP_OAUTH_PASSWORD=')) { 'MCP_OAUTH_PASSWORD=' + (' ' * 24) } else { $_ }
        }
        [IO.File]::WriteAllLines($OAuthFile, [string[]]$WhitespaceLines, $Utf8)
        $Result = Invoke-PowerShellFixture $Bootstrap '-Show'
        Assert-Fixture ($Result.ExitCode -eq 1) 'bootstrap showed whitespace-only credentials'
        $RepairLog = & $Bootstrap 'https://example.test'
        $RepairedLine = [IO.File]::ReadAllLines($OAuthFile) | Where-Object { $_.StartsWith('MCP_OAUTH_PASSWORD=') }
        $Repaired = $RepairedLine.Substring('MCP_OAUTH_PASSWORD='.Length)
        Assert-Fixture (-not [string]::IsNullOrWhiteSpace($Repaired) -and $Repaired.Length -ge 24) 'bootstrap failed to replace a whitespace-only password'
        Assert-Fixture (-not (($RepairLog | Out-String).Contains($Repaired))) 'bootstrap printed the replacement password'

        $BeforeInvalid = [IO.File]::ReadAllText($OAuthFile)
        foreach ($Origin in @('https://user:pass@example.test', 'https://example.test/path', 'https://example.test?query', 'https://example.test#fragment', 'https://example.test\path', 'http://example.test', 'http://127.0.0.2')) {
            $Result = Invoke-PowerShellFixture $Bootstrap ('-PublicOrigin "' + $Origin + '"')
            Assert-Fixture ($Result.ExitCode -eq 2) 'bootstrap accepted an invalid origin'
            Assert-Fixture ([IO.File]::ReadAllText($OAuthFile) -ceq $BeforeInvalid) 'invalid bootstrap modified configuration'
        }
        $env:MCP_OAUTH_USERNAME = 'bad' + [char]10 + 'username'
        $Result = Invoke-PowerShellFixture $Bootstrap '-PublicOrigin "https://example.test"'
        Assert-Fixture ($Result.ExitCode -eq 2) 'bootstrap accepted a multiline username'
        Assert-Fixture ([IO.File]::ReadAllText($OAuthFile) -ceq $BeforeInvalid) 'invalid username modified configuration'
    }
    finally {
        Pop-Location
    }

    # Mock discovery and process startup. Parse the generated command line with
    # Windows itself to verify spaces, brackets and a trailing backslash survive.
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class McpHelperFixtureArguments {
    [DllImport("shell32.dll", SetLastError = true)]
    public static extern IntPtr CommandLineToArgvW([MarshalAs(UnmanagedType.LPWStr)] string commandLine, out int count);
    [DllImport("kernel32.dll")]
    public static extern IntPtr LocalFree(IntPtr memory);
}
'@
    function Test-Path {
        param([string]$LiteralPath)
        return $LiteralPath.EndsWith('chrome.exe')
    }
    function Start-Process {
        param([string]$FilePath, [string[]]$ArgumentList)
        $FixtureState.BrowserArguments = $ArgumentList
    }
    try {
        $env:MCP_BROWSER_PROFILE_DIR = (Join-Path $FixtureRoot 'profile [with spaces]') + '\'
        $ExpectedProfile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($env:MCP_BROWSER_PROFILE_DIR)
        & (Join-Path $RepoRoot 'scripts\start-browser.ps1') | Out-Null
        $Count = 0
        $Pointer = [McpHelperFixtureArguments]::CommandLineToArgvW(('browser.exe ' + ($FixtureState.BrowserArguments -join ' ')), [ref]$Count)
        Assert-Fixture ($Pointer -ne [IntPtr]::Zero) 'Windows could not parse browser arguments'
        try {
            $Arguments = @()
            for ($Index = 0; $Index -lt $Count; $Index++) {
                $Entry = [Runtime.InteropServices.Marshal]::ReadIntPtr($Pointer, $Index * [IntPtr]::Size)
                $Arguments += [Runtime.InteropServices.Marshal]::PtrToStringUni($Entry)
            }
            Assert-Fixture ($Count -eq 4) 'browser profile was split into extra arguments'
            Assert-Fixture ($Arguments[3] -ceq "--user-data-dir=$ExpectedProfile") 'browser profile argument changed'
        }
        finally {
            [void][McpHelperFixtureArguments]::LocalFree($Pointer)
        }
    }
    finally {
        Remove-Item Function:\Test-Path
        Remove-Item Function:\Start-Process
    }

    # Task operations and packaging are stubs. No real task is queried or changed.
    $Installer = Join-Path $Project 'scripts\install-user-service-windows.ps1'
    [IO.File]::Copy((Join-Path $RepoRoot 'scripts\install-user-service-windows.ps1'), $Installer)
    [IO.File]::WriteAllText((Join-Path $Project 'scripts\package-release.ps1'), 'Invoke-FixturePackaging', $Utf8)
    function Get-Command {
        param([string]$Name, [string]$CommandType, [string]$ErrorAction)
        if ($Name -eq 'node' -and $FixtureState.MissingNode) { return $null }
        return [pscustomobject]@{ Source = $FixtureState.HostExecutable }
    }
    function Get-ScheduledTask {
        param([string]$TaskName, [string]$ErrorAction)
        return [pscustomobject]@{ State = $FixtureState.TaskState }
    }
    function Stop-ScheduledTask {
        param([string]$TaskName, [string]$ErrorAction)
        $FixtureState.TaskEvents.Add('stop')
        $FixtureState.TaskState = 'Ready'
    }
    function Start-ScheduledTask {
        param([string]$TaskName, [string]$ErrorAction)
        $FixtureState.TaskEvents.Add('start')
        $FixtureState.TaskState = 'Running'
    }
    function Invoke-FixturePackaging {
        $FixtureState.TaskEvents.Add('package')
        if ($FixtureState.PackageFails) { throw 'fixture packaging failed' }
        $global:LASTEXITCODE = 0
    }
    function New-ScheduledTaskAction {
        param([string]$Execute, [string]$Argument, [string]$WorkingDirectory)
        $FixtureState.TaskArguments = $Argument
        $FixtureState.TaskDirectory = $WorkingDirectory
        return [pscustomobject]@{}
    }
    function New-ScheduledTaskTrigger {
        param([switch]$AtLogOn, [string]$User)
        return [pscustomobject]@{}
    }
    function New-ScheduledTaskSettingsSet {
        param([switch]$AllowStartIfOnBatteries, [switch]$DontStopIfGoingOnBatteries, [timespan]$ExecutionTimeLimit)
        return [pscustomobject]@{}
    }
    function Register-ScheduledTask {
        param([string]$TaskName, $Action, $Trigger, $Settings, [string]$Description, [switch]$Force)
        $FixtureState.TaskEvents.Add('register')
    }

    $env:MCP_BRIDGE_ENV_FILE = $OAuthFile
    $FixtureState.TaskState = 'Running'
    $FixtureState.PackageFails = $true
    $FixtureState.MissingNode = $true
    $Failed = $false
    try { & $Installer -TaskName 'Fixture Only' | Out-Null }
    catch {
        $Failed = $true
        Assert-Fixture ($_.Exception.Message -like '*node is required*') 'installer returned an unexpected preflight failure'
    }
    Assert-Fixture $Failed 'installer accepted a missing dependency'
    Assert-Fixture ($FixtureState.TaskEvents.Count -eq 0) 'installer touched the task before checking dependencies'

    $FixtureState.MissingNode = $false
    $Failed = $false
    try { & $Installer -TaskName 'Fixture Only' | Out-Null }
    catch {
        $Failed = $true
        Assert-Fixture ($_.Exception.Message -like '*fixture packaging failed*') 'installer replaced the original failure'
    }
    Assert-Fixture $Failed 'installer hid a packaging failure'
    Assert-Fixture (($FixtureState.TaskEvents -join ',') -eq 'stop,package,start') 'installer failed to resume the stopped task'
    Assert-Fixture ($FixtureState.TaskState -eq 'Running') 'previous task was left stopped'

    $FixtureState.TaskEvents.Clear()
    $FixtureState.TaskState = 'Ready'
    $FixtureState.PackageFails = $false
    & $Installer -TaskName 'Fixture Only' | Out-Null
    Assert-Fixture (($FixtureState.TaskEvents -join ',') -eq 'package,register,start') 'installer unnecessarily stopped an idle task'
    Assert-Fixture ($FixtureState.TaskDirectory -ceq $Project) 'installer changed a literal working directory'
    $ExpectedRunner = Join-Path $Project 'scripts\run-with-env.ps1'
    Assert-Fixture ($FixtureState.TaskArguments.Contains('-File "' + $ExpectedRunner + '"')) 'installer failed to quote its runner path'
    Assert-Fixture ($FixtureState.TaskArguments.EndsWith('-EnvFile "' + $OAuthFile + '"')) 'installer lost the custom environment file'

    Write-Output 'Windows helper fixtures passed without starting browsers or scheduled tasks.'
}
finally {
    foreach ($Name in $SavedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($Name, $SavedEnvironment[$Name], 'Process')
    }
    if ([IO.Directory]::Exists($FixtureRoot)) {
        [IO.Directory]::Delete($FixtureRoot, $true)
    }
}
