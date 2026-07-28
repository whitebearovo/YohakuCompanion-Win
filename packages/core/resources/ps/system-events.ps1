# System events helper: emits NDJSON lines for session lock/unlock and
# suspend/resume, plus heartbeats. .NET SystemEvents runs its own hidden
# message-pump thread, so a console PowerShell host receives these fine.
# Output schema:
#   {"type":"event","event":"lock"|"unlock"|"suspend"|"resume","at":<epoch ms>}
#   {"type":"heartbeat","at":<epoch ms>}

$ErrorActionPreference = "Stop"

function EpochMs {
  return [long][double]((Get-Date).ToUniversalTime() - (Get-Date "1970-01-01Z").ToUniversalTime()).TotalMilliseconds
}

function Emit([string]$eventName) {
  [Console]::Out.WriteLine((@{ type = "event"; event = $eventName; at = (EpochMs) } | ConvertTo-Json -Compress))
}

$null = Register-ObjectEvent -InputObject ([Microsoft.Win32.SystemEvents]) -EventName SessionSwitch -SourceIdentifier "yohaku.session" -Action {
  $reason = $EventArgs.Reason.ToString()
  $name = $null
  if ($reason -eq "SessionLock") { $name = "lock" }
  elseif ($reason -eq "SessionUnlock") { $name = "unlock" }
  if ($null -ne $name) {
    [Console]::Out.WriteLine((@{ type = "event"; event = $name; at = ([long][double]((Get-Date).ToUniversalTime() - (Get-Date "1970-01-01Z").ToUniversalTime()).TotalMilliseconds) } | ConvertTo-Json -Compress))
  }
}

$null = Register-ObjectEvent -InputObject ([Microsoft.Win32.SystemEvents]) -EventName PowerModeChanged -SourceIdentifier "yohaku.power" -Action {
  $mode = $EventArgs.Mode.ToString()
  $name = $null
  if ($mode -eq "Suspend") { $name = "suspend" }
  elseif ($mode -eq "Resume") { $name = "resume" }
  if ($null -ne $name) {
    [Console]::Out.WriteLine((@{ type = "event"; event = $name; at = ([long][double]((Get-Date).ToUniversalTime() - (Get-Date "1970-01-01Z").ToUniversalTime()).TotalMilliseconds) } | ConvertTo-Json -Compress))
  }
}

try {
  while ($true) {
    # Wait-Event keeps the runspace responsive to the registered actions.
    $null = Wait-Event -Timeout 30
    [Console]::Out.WriteLine((@{ type = "heartbeat"; at = (EpochMs) } | ConvertTo-Json -Compress))
  }
}
finally {
  Unregister-Event -SourceIdentifier "yohaku.session" -ErrorAction SilentlyContinue
  Unregister-Event -SourceIdentifier "yohaku.power" -ErrorAction SilentlyContinue
}
