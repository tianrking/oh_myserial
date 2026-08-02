[CmdletBinding()]
param(
    [string]$BinaryPath
)

$ErrorActionPreference = "Stop"

function Get-FreeLoopbackPort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return $listener.LocalEndpoint.Port
    }
    finally {
        $listener.Stop()
    }
}

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
    $BinaryPath = Join-Path $repoRoot "target\release\ohmyserial.exe"
}
if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
    throw "Release binary was not found: $BinaryPath"
}

$apiPort = Get-FreeLoopbackPort
$tcpPort = Get-FreeLoopbackPort
$stdout = [System.IO.Path]::GetTempFileName()
$stderr = [System.IO.Path]::GetTempFileName()
$process = $null
$client = $null

try {
    $process = Start-Process -FilePath $BinaryPath -ArgumentList @(
        "share", "mock:release-smoke", "--tcp", "1", "--tcp-raw",
        "--tcp-base", "$tcpPort", "--api", "127.0.0.1:$apiPort"
    ) -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr

    $ready = $false
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        Start-Sleep -Milliseconds 100
        try {
            $health = Invoke-RestMethod -UseBasicParsing -Uri "http://127.0.0.1:$apiPort/v1/health" -TimeoutSec 1
            if ($health.ok -eq $true) {
                $ready = $true
                break
            }
        }
        catch {
            if ($process.HasExited) {
                throw "Release process exited early with code $($process.ExitCode)"
            }
        }
    }
    if (-not $ready) {
        throw "Release Hub did not become ready"
    }

    $index = (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$apiPort/" -TimeoutSec 3).Content
    if ($index -notmatch '<div id="root"></div>') {
        throw "Embedded UI root is missing"
    }
    $status = Invoke-RestMethod -UseBasicParsing -Uri "http://127.0.0.1:$apiPort/v1/status" -TimeoutSec 3
    if (-not $status.port.connected) {
        throw "Mock serial port did not connect"
    }
    $endpoints = Invoke-RestMethod -UseBasicParsing -Uri "http://127.0.0.1:$apiPort/v1/endpoints" -TimeoutSec 3
    if ($endpoints.endpoints.Count -lt 3) {
        throw "Endpoint catalog is incomplete"
    }
    $metrics = (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$apiPort/v1/metrics" -TimeoutSec 3).Content
    if ($metrics -notmatch "ohmyserial_port_connected 1") {
        throw "Connected Prometheus gauge is missing"
    }

    $client = [System.Net.Sockets.TcpClient]::new("127.0.0.1", $tcpPort)
    $stream = $client.GetStream()
    $stream.ReadTimeout = 3000
    $payload = [byte[]](0, 1, 2, 255)
    $stream.Write($payload, 0, $payload.Length)
    $received = [byte[]]::new($payload.Length)
    $offset = 0
    while ($offset -lt $received.Length) {
        $count = $stream.Read($received, $offset, $received.Length - $offset)
        if ($count -le 0) {
            throw "Raw TCP closed before the complete echo"
        }
        $offset += $count
    }
    if (-not [System.Linq.Enumerable]::SequenceEqual($payload, $received)) {
        throw "Raw TCP echo mismatch"
    }

    $export = (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$apiPort/v1/events/export" -TimeoutSec 3).Content
    if ([string]::IsNullOrWhiteSpace($export)) {
        throw "Event export is empty"
    }
    Write-Output "RELEASE_SMOKE_OK api=$apiPort tcp=$tcpPort"
}
finally {
    if ($client) {
        $client.Dispose()
    }
    if ($process -and -not $process.HasExited) {
        # This is a short-lived assertion harness. The Rust integration suite
        # covers graceful shutdown; here we only guarantee no process leak.
        Stop-Process -Id $process.Id
        $process.WaitForExit()
    }
    if (Test-Path -LiteralPath $stdout) {
        Remove-Item -LiteralPath $stdout -Force
    }
    if (Test-Path -LiteralPath $stderr) {
        Remove-Item -LiteralPath $stderr -Force
    }
}
