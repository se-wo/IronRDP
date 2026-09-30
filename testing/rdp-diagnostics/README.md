# RDP session diagnostics

`Get-RdpSessionDiagnostics.ps1` reports which transport (TCP/UDP), graphics codec
(RemoteFX / H.264 AVC 4:2:0 / AVC 4:4:4 / HEVC) and quality an RDP session actually uses.
It is handy for comparing what a Windows RDP server negotiates with `mstsc` versus an IronRDP client.

## Usage

Run it **inside the remote session** (on the RDP host) from an elevated PowerShell,
ideally right after (re)connecting and while something moves on screen (e.g. a video):

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\Get-RdpSessionDiagnostics.ps1 -SampleSeconds 15
```

On the client machine, `-Mode Client` inspects the `mstsc` event log and client-side policy.

| Parameter          | Default | Description                                                  |
|--------------------|---------|--------------------------------------------------------------|
| `-Mode`            | `Auto`  | `Server`, `Client` or `Auto` (both)                          |
| `-SampleSeconds`   | `10`    | One-second samples of the RemoteFX counters (`0` disables)   |
| `-EventWindowHours`| `24`    | How far back to search the event logs                       |
| `-OutputPath`      |         | Write the full raw report as JSON                            |
| `-ShowAllCounters` |         | Print every sampled RemoteFX counter                         |

## Data sources

| What                        | Where                                                                                  |
|-----------------------------|----------------------------------------------------------------------------------------|
| Codec profile (AVC/HEVC)    | `RemoteDesktopServices-RdpCoreTS/Operational` event 162                                |
| Hardware (GPU) encoding     | `RemoteDesktopServices-RdpCoreTS/Operational` event 170                                |
| TCP accept / UDP transport  | `RemoteDesktopServices-RdpCoreTS/Operational` events 131 / 135                         |
| Frame rate, quality, skips  | `RemoteFX Graphics` counters (via `Win32_PerfFormattedData_Counters_RemoteFXGraphics`) |
| RTT, bandwidth, loss        | `RemoteFX Network` counters (via `Win32_PerfFormattedData_Counters_RemoteFXNetwork`)   |
| Resolution, color depth     | `WTSQuerySessionInformation` (`WTSClientDisplay`)                                      |
| Graphics/transport policies | `HKLM\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services`                         |

Counters are read through CIM classes, so the script also works on localized (e.g. German) Windows.
Event messages are printed as logged (localized); codec detection keys on the profile names in them.

References:

- [Diagnose graphics performance issues in Remote Desktop](https://learn.microsoft.com/azure/virtual-desktop/remotefx-graphics-performance-counters)
- [Verify GPU acceleration (events 162 / 170)](https://learn.microsoft.com/azure/virtual-desktop/graphics-enable-gpu-acceleration#verify-gpu-acceleration)
- [Graphics encoding over RDP](https://learn.microsoft.com/azure/virtual-desktop/graphics-encoding)
