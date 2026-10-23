<#
.SYNOPSIS
Installs `adk-rust up` as a real Windows service under the current user's account and proves
what spec 3.2 §7.12 and §10 W3 promise: the service control manager starts the kernel as the
user, it logs to an owner-only file in its home, the control commands reach it over the named
pipe without --server, it comes back after it is killed and after a restart with its process
table intact, and --uninstall removes it.

.DESCRIPTION
CI only (.github/workflows/kernel-service.yml): it registers a service and needs an elevated
shell and the current account's password in ADK_SERVICE_PASSWORD.

.PARAMETER Binary
Path to adk-rust.exe built with the scripted-model feature.
#>
param([Parameter(Mandatory = $true)][string]$Binary)

$ErrorActionPreference = 'Stop'
$Binary = (Resolve-Path $Binary).Path
$Work = Join-Path $env:RUNNER_TEMP ([guid]::NewGuid().ToString('N'))
$KernelHome = Join-Path $Work 'kernel home'
$Project = Join-Path $Work 'project'
$Port = 18765
$Script = Join-Path $Work 'script.json'
New-Item -ItemType Directory -Force -Path $Project | Out-Null
Set-Content -Path $Script -Value '[{"text": "done"}]' -Encoding ascii

function Invoke-Adk {
    & $Binary --model-script $Script @args
}

function Write-Step([string]$Text) { Write-Output "`n== $Text" }

function Stop-WithFailure([string]$Text) {
    Write-Output "FAILED: $Text"
    $log = Join-Path $KernelHome 'kernel.log'
    if (Test-Path $log) { Get-Content $log -Tail 40 }
    exit 1
}

function Wait-For([int]$Seconds, [scriptblock]$Condition) {
    for ($i = 0; $i -lt $Seconds; $i++) {
        if (& $Condition) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}

function Test-Answers {
    Invoke-Adk ps --home $KernelHome *> $null
    return $LASTEXITCODE -eq 0
}

function Test-Completed {
    $rows = Invoke-Adk ps --home $KernelHome --state completed 2> $null |
        Where-Object { $_ -match '^[0-9a-f]{8}-' }
    return @($rows).Count -ge 1
}

function Get-KernelPid {
    return (Get-CimInstance Win32_Service -Filter "Name='$Name'").ProcessId
}

Write-Step '--install --print renders the definition and installs nothing'
$definition = Invoke-Adk up --install --print --home $KernelHome --dir $Project --bind "127.0.0.1:$Port"
if ($LASTEXITCODE -ne 0) { Stop-WithFailure '--print failed' }
$definition
$Name = ([regex]::Match(($definition -join "`n"), 'adk-rust-kernel-[0-9a-f]{16}')).Value
if (-not $Name) { Stop-WithFailure 'no service name in the definition' }
if (Get-Service -Name $Name -ErrorAction SilentlyContinue) { Stop-WithFailure '--print registered a service' }
if (Test-Path $KernelHome) { Stop-WithFailure '--print created the kernel home' }

Write-Step '--install registers the service under this account and starts it'
$install = Invoke-Adk up --install --home $KernelHome --dir $Project --bind "127.0.0.1:$Port"
$install
if ($LASTEXITCODE -ne 0) { Stop-WithFailure '--install failed' }
if (-not (($install -join "`n") -match 'The kernel answers on pipe:')) {
    Stop-WithFailure 'the installed kernel did not answer on its pipe'
}
$config = Get-CimInstance Win32_Service -Filter "Name='$Name'"
$config | Format-List Name, StartName, StartMode, State, PathName
if ($config.StartName -notmatch [regex]::Escape($env:USERNAME)) {
    Stop-WithFailure "the service runs as $($config.StartName), not $env:USERNAME"
}
& sc.exe qfailure $Name
Invoke-Adk ps --home $KernelHome
if ($LASTEXITCODE -ne 0) { Stop-WithFailure 'ps did not reach the kernel over its pipe' }
$log = Join-Path $KernelHome 'kernel.log'
$acl = Get-Acl $log
$acl.Access | Format-Table IdentityReference, FileSystemRights, AccessControlType
if (-not $acl.AreAccessRulesProtected -or @($acl.Access).Count -ne 1 -or
    $acl.Access[0].IdentityReference.Value -notmatch [regex]::Escape($env:USERNAME)) {
    Stop-WithFailure 'the log is not owner-only'
}
if (-not (Wait-For 10 { Select-String -Path $log -Pattern 'ADK-Rust kernel up' -Quiet })) {
    Stop-WithFailure "the kernel's banner is not in its log"
}

Write-Step 'a process the kernel runs survives what follows'
Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$Port/api/kernel/processes" `
    -ContentType 'application/json' -Body '{"target": "adk-rust", "input": {"text": "say done"}}' |
    ConvertTo-Json -Depth 3
if (-not (Wait-For 60 { Test-Completed })) { Stop-WithFailure 'the spawned process did not complete' }

Write-Step 'a killed kernel is restarted by the failure actions'
$first = Get-KernelPid
if (-not $first) { Stop-WithFailure 'no kernel process' }
Stop-Process -Id $first -Force
$restarted = Wait-For 90 { $p = Get-KernelPid; $p -and $p -ne $first -and (Test-Answers) }
if (-not $restarted) { Stop-WithFailure 'the kernel was not restarted after it was killed' }
if (-not (Test-Completed)) { Stop-WithFailure 'the completed process is gone after the restart' }

Write-Step 'a stop through the service control manager stops the kernel, and a restart keeps the process table'
$before = Get-KernelPid
$took = Measure-Command { Restart-Service -Name $Name }
Write-Output "restart took $([int]$took.TotalSeconds) s"
if ($took.TotalSeconds -gt 60) { Stop-WithFailure 'the kernel did not stop on the service stop' }
if (-not (Wait-For 60 { $p = Get-KernelPid; $p -and $p -ne $before -and (Test-Answers) })) {
    Stop-WithFailure 'the kernel does not answer after a restart'
}
if (-not (Test-Completed)) { Stop-WithFailure 'the completed process is gone after the service restart' }

Write-Step '--uninstall removes the service and keeps the home'
Invoke-Adk up --uninstall --home $KernelHome
if ($LASTEXITCODE -ne 0) { Stop-WithFailure '--uninstall failed' }
if (-not (Wait-For 30 { -not (Get-Service -Name $Name -ErrorAction SilentlyContinue) })) {
    Stop-WithFailure 'the service is still registered'
}
if (Test-Answers) { Stop-WithFailure 'a kernel still answers' }
if (-not ((Test-Path $log) -and (Test-Path (Join-Path $KernelHome 'harness.db')))) {
    Stop-WithFailure 'the kernel home was not kept'
}

Write-Step 'the kernel service installed, restarted and uninstalled as the user'
exit 0
