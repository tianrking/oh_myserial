# Windows COM bridge

`ohmyserial` can own a physical `COMx` port directly. Some legacy programs,
however, can only open a local COM device and cannot speak TCP, WebSocket, or
RFC2217. The `bridge-com` command covers that workflow by forwarding an
existing COM endpoint to one of the Hub's raw TCP fan-out listeners.

The bridge is a user-mode data path:

```text
legacy app (COM12) <-> com0com pair <-> bridge-com (COM13) <-> TCP :8788 <-> ohmyserial Hub <-> real UART
```

The repository does not ship a signed Windows kernel virtual-COM driver. A
kernel driver is an OS-level installation and signing responsibility; the
bridge intentionally integrates with an installed provider such as
[com0com](https://github.com/Valley-of-Doom/Null-modem-emulator) instead of
silently claiming to create one.

## 1. Create a paired COM endpoint

Install a current, signed com0com build appropriate for the machine. The
administrator setup tool should create a pair such as `COM12 <-> COM13`.
Choose the endpoint that the legacy application will open and keep the other
endpoint for `bridge-com`. The included PowerShell helper prints the required
steps and can optionally call a local `setupc.exe` when its path is supplied:

```powershell
./scripts/windows-com0com-setup.ps1 -SetupC 'C:\Program Files\com0com\setupc.exe'
```

If the driver package uses a different installer, create the pair in its GUI
or administrator command line and then continue with the verification below.

## 2. Start the Hub

```powershell
ohmyserial.exe share COM3 --tcp 1 --tcp-raw --tcp-base 8788 --api 127.0.0.1:8787
```

The default raw TCP endpoint is `127.0.0.1:8788`. Confirm it in the embedded
console's **Endpoints** tab or with:

```powershell
Invoke-RestMethod http://127.0.0.1:8787/v1/endpoints
```

## 3. Start the bridge

Use the endpoint that belongs to the bridge side of the pair:

```powershell
ohmyserial.exe bridge-com COM13 --tcp 127.0.0.1:8788 `
  --baud 115200 --data-bits 8 --parity none --stop-bits 1 --flow-control none
```

Now configure PuTTY, SSCOM, a vendor utility, or another COM-only program to
open `COM12` with the same line settings. RX is broadcast through the Hub;
TX is arbitrated by the Hub's normal queue/lease policy. Closing either the
TCP connection or the COM application stops the bridge cleanly.

## Diagnostics and boundaries

- `list-ports` should show both physical and virtual COM endpoints before the
  bridge is started.
- Keep the Hub and bridge bound to loopback unless a separately secured tunnel
  is in place. Raw TCP has no authentication of its own.
- RFC2217 is an alternative for network-aware tools; it is not required for
  the COM bridge.
- `bridge-com` does not install drivers, create device names, or bypass Windows
  driver signing. Use the vendor/com0com documentation for driver lifecycle.
