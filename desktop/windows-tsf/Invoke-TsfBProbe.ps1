# Run only in a disposable, interactive Windows 11 standard-user account.
# This explicit B experiment never ships in the app or installer.
param(
  [ValidateSet('0404', '0409')]
  [string]$Language = '0409'
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$scratch = Join-Path $repo '.scratch/tsf-b'
$tempRoot = Join-Path $repo '.scratch/tsf-temp'
New-Item -ItemType Directory -Force -Path $scratch, $tempRoot | Out-Null
$env:TEMP = $tempRoot
$env:TMP = $tempRoot
$release = Join-Path $repo '.scratch/tsf-native/build/Release'
$registrar = Join-Path $release 'voicetype_tsf_registrar.exe'
$target = Join-Path $release 'voicetype_tsf_b_target.exe'
$dll = Join-Path $release 'voicetype_tsf_probe.dll'
foreach ($path in @($registrar, $target, $dll)) {
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
    throw "Build the isolated TSF probe first: $path"
  }
}

function Read-KeyboardIdentity {
  $status = & $registrar status
  if ($LASTEXITCODE -ne 0) { throw 'TSF keyboard status query failed' }
  $identity = $status | Where-Object { $_ -like 'keyboard_identity=*' } |
    Select-Object -First 1
  if (-not $identity) { throw 'Cannot establish the current keyboard profile' }
  return $identity
}

function Read-OwnState {
  $status = & $registrar status $dll $Language
  if ($LASTEXITCODE -ne 0) { throw 'TSF own-state query failed' }
  $line = $status | Where-Object { $_ -like 'own_state *' } |
    Select-Object -First 1
  if (-not $line) {
    throw "TSF own-state frame missing: $($status -join ' ')"
  }
  if (-not ($line -match '^own_state observed=(-?\d+) com_owned=(\d+) com_conflict=(\d+) profile=(-?\d+) category=(-?\d+) active=(-?\d+)$')) {
    throw "TSF own-state frame missing or malformed: $($status -join ' ')"
  }
  return @{
    observed = [int]$Matches[1]
    com_owned = [int]$Matches[2]
    com_conflict = [int]$Matches[3]
    profile = [int]$Matches[4]
    category = [int]$Matches[5]
    active = [int]$Matches[6]
  }
}

function New-ProbeLog {
  return (Join-Path $scratch "target-$PID-$([guid]::NewGuid().ToString('N')).log")
}

$before = Read-KeyboardIdentity
$issues = [System.Collections.Generic.List[string]]::new()
$registrationAttempted = $false
$activationAttempted = $false
try {
  $initial = Read-OwnState
  if ($initial.observed -ne 1 -or $initial.com_owned -ne 0 -or
      $initial.com_conflict -ne 0 -or $initial.profile -ne 0 -or
      $initial.category -ne 0 -or $initial.active -ne 0) {
    throw 'Probe state was not clean before registration; refusing to mutate it'
  }
  $registrationAttempted = $true
  & $registrar register $dll $Language
  if ($LASTEXITCODE -ne 0) { throw 'Standard-user speech registration failed' }
  $activationAttempted = $true
  & $registrar activate $dll $Language
  if ($LASTEXITCODE -ne 0) { throw 'Speech activation request failed' }
  $log = New-ProbeLog
  & $target $log
  if ($LASTEXITCODE -ne 0) { throw 'New x64 target did not observe TIP HELLO/context' }
  Write-Host "B synthetic new-process probe passed; metadata: $log"
} catch {
  $issues.Add("Probe: $($_.Exception.Message)")
} finally {
  # A failed register may still have installed a partial TSF profile. Query
  # actual owned state and run the registrar's guarded, retryable cleanup.
  $state = $null
  try { $state = Read-OwnState }
  catch { $issues.Add("Pre-cleanup state: $($_.Exception.Message)") }
  if ($activationAttempted) {
    & $registrar deactivate $dll $Language
    if ($LASTEXITCODE -ne 0) {
      $issues.Add("FORSESSION deactivation failed with exit $LASTEXITCODE")
    }
  }
  if ($registrationAttempted -and ($null -eq $state -or $state.com_owned -eq 1)) {
    & $registrar unregister $dll $Language
    if ($LASTEXITCODE -ne 0) {
      $issues.Add("Unregister failed with exit $LASTEXITCODE; inspect owned TSF state")
    }
  }
  $afterState = $null
  try { $afterState = Read-OwnState }
  catch { $issues.Add("Post-cleanup state: $($_.Exception.Message)") }
  if ($null -eq $afterState -or $afterState.observed -ne 1 -or
      $afterState.com_owned -ne 0 -or $afterState.com_conflict -ne 0 -or
      $afterState.profile -ne 0 -or $afterState.category -ne 0 -or
      $afterState.active -ne 0) {
    $issues.Add('Probe COM/profile/category/active state is not confirmed absent')
  }
  # A new process after cleanup must not auto-load this speech TIP. This is
  # separate from the registrar's public-API S_OK status and keyboard check.
  $absentLog = New-ProbeLog
  & $target $absentLog --expect-absent
  if ($LASTEXITCODE -ne 0) {
    $issues.Add("Speech TIP still loaded after cleanup; metadata: $absentLog")
  }
  try {
    $after = Read-KeyboardIdentity
    if ($after -ne $before) { $issues.Add('Keyboard/IME profile changed') }
  } catch { $issues.Add("Keyboard after cleanup: $($_.Exception.Message)") }
}
if ($issues.Count -ne 0) { throw ($issues -join '; ') }
Write-Host 'B cleanup checks passed; physical Ctrl+CapsLock and other apps remain manual gates'
