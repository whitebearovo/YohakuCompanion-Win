# SMTC fallback provider: polls Windows.Media.Control every second and emits
# one NDJSON line per change, plus a heartbeat every 30 seconds.
# Output schema:
#   {"type":"media","session":{...}|null,"at":<epoch ms>}
#   {"type":"heartbeat","at":<epoch ms>}
# PowerShell 5.1 compatible (WinRT via reflection).

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Runtime.WindowsRuntime

$asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
  })[0]

function Await($WinRtTask, $ResultType) {
  $asTask = $asTaskGeneric.MakeGenericMethod($ResultType)
  $netTask = $asTask.Invoke($null, @($WinRtTask))
  $null = $netTask.Wait(5000)
  if (-not $netTask.IsCompleted) { throw "WinRT await timeout" }
  return $netTask.Result
}

$null = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager, Windows.Media.Control, ContentType = WindowsRuntime]
$mgrType = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager]
$manager = Await ($mgrType::RequestAsync()) $mgrType

function EpochMs {
  return [long][double]((Get-Date).ToUniversalTime() - (Get-Date "1970-01-01Z").ToUniversalTime()).TotalMilliseconds
}

function ReadSession($manager) {
  $session = $manager.GetCurrentSession()
  if ($null -eq $session) { return $null }
  $propsType = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionMediaProperties]
  $props = Await ($session.TryGetMediaPropertiesAsync()) $propsType
  $timeline = $session.GetTimelineProperties()
  $playback = $session.GetPlaybackInfo()
  $kind = "unknown"
  if ($null -ne $playback.PlaybackType) {
    switch ([int]$playback.PlaybackType.Value) {
      1 { $kind = "music" }
      3 { $kind = "video" }
    }
  }
  return [ordered]@{
    sourceAppId = $session.SourceAppUserModelId
    title       = $props.Title
    artist      = $props.Artist
    album       = $props.AlbumTitle
    kind        = $kind
    playing     = ([int]$playback.PlaybackStatus -eq 4)
    duration    = [double]$timeline.EndTime.TotalSeconds
    position    = [double]$timeline.Position.TotalSeconds
    updatedAt   = [long]($timeline.LastUpdatedTime.ToUnixTimeMilliseconds())
  }
}

$lastJson = ""
$lastHeartbeat = 0
while ($true) {
  try {
    $session = ReadSession $manager
    $payload = @{ type = "media"; session = $session; at = (EpochMs) }
    $json = ($payload | ConvertTo-Json -Compress -Depth 6)
    # Emit only when the session content changed (position changes included:
    # the Node side deduplicates semantics; volume here is 1 line/second max).
    if ($json -ne $lastJson) {
      $lastJson = $json
      [Console]::Out.WriteLine($json)
    }
  }
  catch {
    # Manager may need re-acquisition after failures.
    try { $manager = Await ($mgrType::RequestAsync()) $mgrType } catch {}
  }
  $now = EpochMs
  if ($now - $lastHeartbeat -ge 30000) {
    $lastHeartbeat = $now
    [Console]::Out.WriteLine((@{ type = "heartbeat"; at = $now } | ConvertTo-Json -Compress))
  }
  Start-Sleep -Milliseconds 1000
}
