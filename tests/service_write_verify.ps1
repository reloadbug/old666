# old666 real-machine write verification (manual, NOT part of CI).
#
# Verifies, against the real machine:
#   * delayed auto-start via SCM ChangeServiceConfig2 -> DelayedAutostart 0->1
#   * rollback of that change back to the original state
#   * disable via ChangeServiceConfigW               -> Start 2->4
#   * rollback of the disable                        -> Start back to 2
#
# Safety:
#   * creates its OWN throwaway service (old666_delay_probe) that is never
#     started, and deletes it at the end;
#   * everything else on the machine is untouched;
#   * the CLI path is the same one the GUI uses, behind --allow-write;
#   * all test artifacts go to %TEMP%\old666-write-verify (OLD666_DATA_DIR),
#     never into the repo or the real app data dir.
#
# Run:  powershell -ExecutionPolicy Bypass -File tests\service_write_verify.ps1
# (PowerShell 5.1 reads .ps1 as ANSI, hence ASCII-only source.)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$svcName  = "old666_delay_probe"
$dispName = "old666 updater probe"   # matches a SAFE keyword so the risk gate passes
$binPath  = "C:\Program Files\7-Zip\7z.exe"
$regKey   = "HKLM:\SYSTEM\CurrentControlSet\Services\$svcName"
$poc      = Join-Path $PSScriptRoot "..\src-tauri\target\debug\old666-poc.exe"

# isolate every write this test produces
$env:OLD666_DATA_DIR = Join-Path $env:TEMP "old666-write-verify"

function Get-Delayed {
    $v = (Get-Item $regKey).GetValue("DelayedAutostart")
    if ($null -eq $v) { return 0 } else { return $v }
}
function Get-Start { (Get-Item $regKey).GetValue("Start") }

function Apply-Change($action) {
    & $poc --startup-apply --action $action --name $dispName --allow-write 2>&1 | Out-String
}
function Restore-Batch($batch) {
    if ($batch) { & $poc --startup-restore $batch 2>&1 | Out-Null }
}
function Batch-Of($text) {
    if ($text -match "(sb-[0-9a-fA-F-]+)") { return $Matches[1] }
    return ""
}

$created = $false
try {
    if (-not (Test-Path $regKey)) {
        sc.exe create $svcName type= own start= auto binPath= "$binPath" displayname= "$dispName" | Out-Null
        $created = $true
        Start-Sleep -Milliseconds 300
    }

    $rounds = 3
    $delayOk = 0; $rollbackOk = 0
    for ($i = 1; $i -le $rounds; $i++) {
        Write-Output "---- delay round $i ----"
        $before = Get-Delayed
        $out = Apply-Change "delay"
        Write-Output $out
        $batch = Batch-Of $out
        $mid = Get-Delayed
        Write-Output "  DelayedAutostart: $before -> $mid"
        if ($mid -eq 1 -and $batch) { $delayOk++ }
        Restore-Batch $batch
        $after = Get-Delayed
        Write-Output "  after restore: $after"
        if ($after -eq $before) { $rollbackOk++ }
    }
    Write-Output "delay applied : $delayOk/$rounds"
    Write-Output "rollback ok   : $rollbackOk/$rounds"

    Write-Output "---- disable round ----"
    $s0 = Get-Start
    $out = Apply-Change "disable"
    Write-Output $out
    $batch = Batch-Of $out
    Write-Output "  Start: $s0 -> $(Get-Start)"
    Restore-Batch $batch
    Write-Output "  after restore: $(Get-Start)"
}
finally {
    if ($created) { sc.exe delete $svcName | Out-Null }
}
