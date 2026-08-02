[CmdletBinding()]
param(
    [string]$SetupC,
    [string]$PortA = "COM12",
    [string]$PortB = "COM13"
)

$ErrorActionPreference = "Stop"

Write-Host "ohmyserial Windows COM bridge setup"
Write-Host "This helper does not install a kernel driver. Install a signed com0com build first."
Write-Host "The legacy application will use $PortA; ohmyserial bridge-com will use $PortB."

if ([string]::IsNullOrWhiteSpace($SetupC)) {
    Write-Host "Create the pair with your installed com0com tool, then run:"
    Write-Host "  ohmyserial.exe bridge-com $PortB --tcp 127.0.0.1:8788"
    exit 0
}

$resolved = (Resolve-Path -LiteralPath $SetupC -ErrorAction Stop).Path
if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
    throw "setupc.exe was not found at $resolved"
}

Write-Host "Creating pair $PortA <-> $PortB with $resolved"
& $resolved install PortName=$PortA PortName=$PortB
if ($LASTEXITCODE -ne 0) {
    throw "com0com setupc.exe returned exit code $LASTEXITCODE"
}

Write-Host "Pair created. Verify it in Device Manager and with:"
Write-Host "  ohmyserial.exe list-ports"
Write-Host "Then start:"
Write-Host "  ohmyserial.exe bridge-com $PortB --tcp 127.0.0.1:8788"
