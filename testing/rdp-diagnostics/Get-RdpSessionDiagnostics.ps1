<#
.SYNOPSIS
    Reports which transport, graphics codec and quality settings an RDP session actually uses.

.DESCRIPTION
    Collects RDP diagnostics from the local Windows machine and prints a readable report.

    Server side (run INSIDE the remote session / on the RDP host, elevated):
      - Current session: protocol, client name/address, resolution and color depth (WTS API)
      - Live RemoteFX Graphics counters: input/output FPS, frame quality, encoding time, skipped frames
      - Live RemoteFX Network counters: TCP/UDP RTT, bandwidth, loss rate
      - RdpCoreTS events: accepted TCP connection (131), multitransport/UDP (135),
        codec profile AVC420/AVC444/HEVC (162), hardware encoder (170)
      - Graphics / transport group policies (AVC444, HW encoding, transport selection, ...)
      - Security layer and established RDP connections

    Client side (run on the machine running mstsc / Windows App):
      - TerminalServices-RDPClient events (connection, multitransport/UDP)
      - Client-side policies (e.g. UDP disabled on client)

    All performance counter access goes through the language-independent
    Win32_PerfFormattedData_Counters_* CIM classes, so the script works on localized
    (e.g. German) Windows installations. Event messages are printed as logged, i.e. localized.

.PARAMETER Mode
    Auto (default) runs every section and skips what is not applicable.
    Server / Client restrict the report to the respective side.

.PARAMETER SampleSeconds
    How many one-second samples of the RemoteFX counters to take. Generate screen activity
    (e.g. play a video, move a window) while sampling, otherwise frame rates stay at 0.

.PARAMETER EventWindowHours
    How far back to look in the event logs.

.PARAMETER Export
    Export the report to a new folder on the desktop (RdpDiagnostics_<computer>_<timestamp>)
    and pack it into a .zip next to it. The desktop is the one of the user signed in to the
    session, even when the elevated shell runs under a different admin account:
      Report.txt    - the console report as shown
      Report.json   - the full raw data
      Events.csv    - all collected events (Excel-ready, list separator of the current culture)
      Counters.csv  - sampled RemoteFX counters (avg/min/max/last per counter)

.PARAMETER OutputDirectory
    Export into this folder instead of the desktop. Implies -Export.

.PARAMETER ShowAllCounters
    Also print every sampled RemoteFX counter, not only the summarized ones.

.EXAMPLE
    # Inside the remote session, elevated PowerShell:
    .\Get-RdpSessionDiagnostics.ps1 -SampleSeconds 15

.EXAMPLE
    .\Get-RdpSessionDiagnostics.ps1 -SampleSeconds 15 -Export

.EXAMPLE
    .\Get-RdpSessionDiagnostics.ps1 -Mode Client -EventWindowHours 2 -OutputDirectory C:\Temp\rdp-client
#>
[CmdletBinding()]
param(
    [ValidateSet('Auto', 'Server', 'Client')]
    [string] $Mode = 'Auto',

    [ValidateRange(0, 600)]
    [int] $SampleSeconds = 10,

    [ValidateRange(1, 720)]
    [int] $EventWindowHours = 24,

    [switch] $Export,

    [string] $OutputDirectory,

    [switch] $ShowAllCounters
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$report = [ordered]@{
    Timestamp = (Get-Date).ToString('o')
    Computer  = $env:COMPUTERNAME
}

$policyPath = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'
$clientPolicyPath = Join-Path $policyPath 'Client'
$terminalServerPath = 'HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server'
$rdpTcpPath = Join-Path $terminalServerPath 'WinStations\RDP-Tcp'
$rdpCoreLog = 'Microsoft-Windows-RemoteDesktopServices-RdpCoreTS/Operational'
$rdpClientLog = 'Microsoft-Windows-TerminalServices-RDPClient/Operational'
$eventStart = (Get-Date).AddHours(-$EventWindowHours)

# Shadows Write-Host for the rest of the script so every line of the console report is also kept
# for Report.txt; output still goes to the real Write-Host unchanged.
$reportLines = New-Object System.Collections.Generic.List[string]
$reportLine = New-Object System.Text.StringBuilder
function Write-Host {
    [CmdletBinding()]
    param(
        [Parameter(Position = 0, ValueFromPipeline)] [object] $Object = '',
        [switch] $NoNewline,
        [ConsoleColor] $ForegroundColor
    )

    process {
        Microsoft.PowerShell.Utility\Write-Host @PSBoundParameters
        $null = $reportLine.Append("$Object")
        if (-not $NoNewline) {
            $reportLines.Add($reportLine.ToString())
            $null = $reportLine.Clear()
        }
    }
}

function Write-Section {
    param([Parameter(Mandatory)] [string] $Title)

    Write-Host ''
    Write-Host ('=' * 78) -ForegroundColor DarkCyan
    Write-Host " $Title" -ForegroundColor Cyan
    Write-Host ('=' * 78) -ForegroundColor DarkCyan
}

function Write-Finding {
    param(
        [Parameter(Mandatory)] [string] $Label,
        [AllowNull()] [object] $Value,
        [string] $Color = 'White'
    )

    if ($null -eq $Value -or "$Value" -eq '') {
        $Value = '-'
    }
    Write-Host ('  {0,-38} ' -f $Label) -NoNewline -ForegroundColor Gray
    Write-Host $Value -ForegroundColor $Color
}

function Get-RegistryValues {
    param([Parameter(Mandatory)] [string] $Path)

    $result = [ordered]@{}
    $item = Get-ItemProperty -Path $Path -ErrorAction SilentlyContinue
    if ($null -eq $item) {
        return $result
    }
    foreach ($property in $item.PSObject.Properties) {
        if ($property.Name -notlike 'PS*') {
            $result[$property.Name] = $property.Value
        }
    }
    return $result
}

function Get-RecentEvents {
    param(
        [Parameter(Mandatory)] [string] $LogName,
        [int[]] $Id
    )

    $filter = @{ LogName = $LogName; StartTime = $eventStart }
    if ($Id) {
        $filter['Id'] = $Id
    }
    try {
        return @(Get-WinEvent -FilterHashtable $filter -ErrorAction Stop)
    }
    catch {
        # "No events were found" is reported as an exception; everything else is worth surfacing
        if ($_.FullyQualifiedErrorId -notlike 'NoMatchingEventsFound*') {
            Write-Warning "Could not read '$LogName': $($_.Exception.Message)"
        }
        return @()
    }
}

function ConvertTo-EventRecord {
    param([Parameter(Mandatory)] [System.Diagnostics.Eventing.Reader.EventRecord] $EventRecord)

    $message = $null
    try {
        $message = $EventRecord.FormatDescription()
    }
    catch {
        $message = $null
    }
    [pscustomobject]@{
        Time       = $EventRecord.TimeCreated
        Id         = $EventRecord.Id
        Message    = if ($message) { ($message -replace '\s+', ' ').Trim() } else { '' }
        Properties = @($EventRecord.Properties | ForEach-Object { $_.Value })
    }
}

function Write-EventList {
    param(
        [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Records,
        [int] $Max = 10
    )

    if ($Records.Count -eq 0) {
        Write-Host '  (no events in the selected time window)' -ForegroundColor DarkGray
        return
    }
    foreach ($record in ($Records | Select-Object -First $Max)) {
        $text = $record.Message
        if (-not $text) {
            $text = 'Properties: ' + ($record.Properties -join ' | ')
        }
        Write-Host ('  [{0:yyyy-MM-dd HH:mm:ss}] {1,5}  ' -f $record.Time, $record.Id) -NoNewline -ForegroundColor DarkGray
        Write-Host $text
    }
    if ($Records.Count -gt $Max) {
        Write-Host "  ... $($Records.Count - $Max) older event(s) omitted" -ForegroundColor DarkGray
    }
}

function Get-EventText {
    param([Parameter(Mandatory)] [object] $Record)

    return ($Record.Message + ' ' + ($Record.Properties -join ' '))
}

function Measure-PerfClass {
    param(
        [Parameter(Mandatory)] [string] $ClassName,
        [Parameter(Mandatory)] [int] $Samples
    )

    try {
        $null = Get-CimInstance -ClassName $ClassName -ErrorAction Stop
    }
    catch {
        Write-Host "  Counter class $ClassName is not available on this machine" -ForegroundColor DarkGray
        return @()
    }

    # instance name -> property name -> list of samples
    $data = @{}
    for ($i = 0; $i -lt [Math]::Max($Samples, 1); $i++) {
        if ($i -gt 0) {
            Start-Sleep -Seconds 1
        }
        foreach ($instance in @(Get-CimInstance -ClassName $ClassName)) {
            if ($instance.Name -eq '_Total') {
                continue
            }
            if (-not $data.ContainsKey($instance.Name)) {
                $data[$instance.Name] = [ordered]@{}
            }
            foreach ($property in $instance.CimInstanceProperties) {
                if ($property.Name -in @('Name', 'Caption', 'Description') -or
                    $property.Name -like 'Frequency_*' -or $property.Name -like 'Timestamp_*' -or
                    $null -eq $property.Value) {
                    continue
                }
                $bucket = $data[$instance.Name]
                if (-not $bucket.Contains($property.Name)) {
                    $bucket[$property.Name] = New-Object System.Collections.Generic.List[double]
                }
                $bucket[$property.Name].Add([double] $property.Value)
            }
        }
    }

    $result = @()
    foreach ($instanceName in $data.Keys) {
        foreach ($propertyName in $data[$instanceName].Keys) {
            $values = $data[$instanceName][$propertyName]
            $stats = $values | Measure-Object -Average -Maximum -Minimum
            $result += [pscustomobject]@{
                Instance = $instanceName
                Counter  = $propertyName
                Avg      = [Math]::Round($stats.Average, 2)
                Min      = $stats.Minimum
                Max      = $stats.Maximum
                Last     = $values[$values.Count - 1]
            }
        }
    }
    return $result
}

function Get-CounterValue {
    param(
        [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Rows,
        [Parameter(Mandatory)] [string] $Instance,
        [Parameter(Mandatory)] [string] $Counter,
        [string] $Field = 'Avg'
    )

    # $Counter is a wildcard pattern: WMI property names are derived from the localized-neutral
    # counter names and differ slightly between Windows builds
    $row = $Rows | Where-Object { $_.Instance -eq $Instance -and $_.Counter -like $Counter } | Select-Object -First 1
    if ($null -eq $row) {
        return $null
    }
    return $row.$Field
}

function Get-WtsSessionInfo {
    # WTS_INFO_CLASS values: https://learn.microsoft.com/windows/win32/api/wtsapi32/ne-wtsapi32-wts_info_class
    if (-not ('IronRdpDiag.Wts' -as [type])) {
        Add-Type -Namespace IronRdpDiag -Name Wts -MemberDefinition @'
[DllImport("wtsapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern bool WTSQuerySessionInformationW(IntPtr hServer, int sessionId, int infoClass, out IntPtr buffer, out int bytesReturned);
[DllImport("wtsapi32.dll")]
public static extern void WTSFreeMemory(IntPtr memory);
'@
    }

    $sessionId = (Get-Process -Id $PID).SessionId
    $query = {
        param([int] $InfoClass)
        $buffer = [IntPtr]::Zero
        $length = 0
        if (-not [IronRdpDiag.Wts]::WTSQuerySessionInformationW([IntPtr]::Zero, $sessionId, $InfoClass, [ref] $buffer, [ref] $length)) {
            return $null
        }
        try {
            $bytes = New-Object byte[] $length
            [System.Runtime.InteropServices.Marshal]::Copy($buffer, $bytes, 0, $length)
            return , $bytes
        }
        finally {
            [IronRdpDiag.Wts]::WTSFreeMemory($buffer)
        }
    }
    $asString = {
        param([byte[]] $Bytes)
        if ($null -eq $Bytes) { return $null }
        return [System.Text.Encoding]::Unicode.GetString($Bytes).TrimEnd([char] 0)
    }

    $protocolBytes = & $query 16 # WTSClientProtocolType
    $displayBytes = & $query 15  # WTSClientDisplay
    $addressBytes = & $query 14  # WTSClientAddress

    $protocol = if ($protocolBytes) {
        switch ([BitConverter]::ToUInt16($protocolBytes, 0)) {
            0 { 'Console (local session)' }
            1 { 'ICA' }
            2 { 'RDP' }
            default { "Unknown ($_)" }
        }
    }

    $display = $null
    if ($displayBytes -and $displayBytes.Length -ge 12) {
        $colorCode = [BitConverter]::ToUInt32($displayBytes, 8)
        $bpp = switch ($colorCode) {
            1 { 4 } 2 { 8 } 4 { 16 } 8 { 24 } 16 { 15 } 24 { 24 } 32 { 32 } default { "code $colorCode" }
        }
        $display = [ordered]@{
            Width        = [BitConverter]::ToUInt32($displayBytes, 0)
            Height       = [BitConverter]::ToUInt32($displayBytes, 4)
            BitsPerPixel = $bpp
        }
    }

    $address = $null
    if ($addressBytes -and $addressBytes.Length -ge 8) {
        # WTS_CLIENT_ADDRESS: DWORD AddressFamily; BYTE Address[20] (IPv4 starts at offset 2)
        $family = [BitConverter]::ToUInt32($addressBytes, 0)
        if ($family -eq 2) {
            $address = '{0}.{1}.{2}.{3}' -f $addressBytes[6], $addressBytes[7], $addressBytes[8], $addressBytes[9]
        }
        elseif ($family -ne 0) {
            $address = "address family $family"
        }
    }

    [ordered]@{
        SessionId     = $sessionId
        SessionName   = & $asString (& $query 6) # WTSWinStationName
        Protocol      = $protocol
        ClientName    = & $asString (& $query 10) # WTSClientName
        ClientAddress = $address
        ClientBuild   = $(
            $b = & $query 9 # WTSClientBuildNumber
            if ($b) { [BitConverter]::ToUInt32($b, 0) }
        )
        Display       = $display
    }
}

function Get-SessionUserDesktop {
    # An elevated shell may run under a different (admin) account than the user signed in to this
    # session, so the desktop is resolved for the owner of explorer.exe in the current session
    $sessionId = (Get-Process -Id $PID).SessionId
    $explorer = Get-CimInstance -ClassName Win32_Process -Filter "Name = 'explorer.exe' AND SessionId = $sessionId" |
        Select-Object -First 1
    if ($null -ne $explorer) {
        $sid = (Invoke-CimMethod -InputObject $explorer -MethodName GetOwnerSid).Sid
        if ($sid) {
            $profilePath = $null
            $profileKey = Get-Item -LiteralPath "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\$sid" -ErrorAction SilentlyContinue
            if ($profileKey) {
                $profilePath = $profileKey.GetValue('ProfileImagePath')
            }

            # "User Shell Folders" holds the (possibly redirected, e.g. OneDrive) desktop as REG_EXPAND_SZ.
            # It must be expanded with the session user's profile, not with this process' environment.
            $candidates = @()
            $userShellFolders = Get-Item -LiteralPath "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders" -ErrorAction SilentlyContinue
            if ($userShellFolders) {
                $raw = $userShellFolders.GetValue('Desktop', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                if ($raw -and $profilePath) {
                    $candidates += [Environment]::ExpandEnvironmentVariables(($raw -ireplace '%USERPROFILE%', $profilePath))
                }
            }
            $shellFolders = Get-Item -LiteralPath "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders" -ErrorAction SilentlyContinue
            if ($shellFolders) {
                $candidates += $shellFolders.GetValue('Desktop')
            }
            if ($profilePath) {
                $candidates += Join-Path $profilePath 'Desktop'
            }
            foreach ($candidate in $candidates) {
                if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Container)) {
                    return $candidate
                }
            }
        }
    }
    return [Environment]::GetFolderPath('Desktop')
}

$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Warning 'Not running elevated: RdpCoreTS event logs and some counters may be unreadable.'
}

$runServer = $Mode -in @('Auto', 'Server')
$runClient = $Mode -in @('Auto', 'Client')

if ($runServer) {
    $serverReport = [ordered]@{}

    # --- Session -----------------------------------------------------------------------------
    Write-Section 'Current session (server side)'
    try {
        $session = Get-WtsSessionInfo
        $serverReport['Session'] = $session
        Write-Finding 'Session' ("{0} (id {1})" -f $session.SessionName, $session.SessionId)
        Write-Finding 'Protocol' $session.Protocol
        Write-Finding 'Client name / address' ("{0} / {1}" -f $session.ClientName, $session.ClientAddress)
        Write-Finding 'Client build' $session.ClientBuild
        if ($session.Display) {
            Write-Finding 'Resolution / color depth' ('{0}x{1} @ {2} bpp' -f $session.Display.Width, $session.Display.Height, $session.Display.BitsPerPixel)
        }
        if ($session.Protocol -ne 'RDP') {
            Write-Host '  This shell is not running inside an RDP session. Codec and quality counters are' -ForegroundColor Yellow
            Write-Host '  only produced on the RDP host, so run the script inside the remote session.' -ForegroundColor Yellow
        }
    }
    catch {
        Write-Warning "Could not query session information: $($_.Exception.Message)"
    }

    # --- Codec / graphics events -------------------------------------------------------------
    Write-Section 'Graphics codec (RdpCoreTS events 162 / 170)'
    $codecEvents = @(Get-RecentEvents -LogName $rdpCoreLog -Id 162, 170 | ForEach-Object { ConvertTo-EventRecord $_ })
    $serverReport['CodecEvents'] = $codecEvents

    $last162 = $codecEvents | Where-Object Id -EQ 162 | Select-Object -First 1
    $last170 = $codecEvents | Where-Object Id -EQ 170 | Select-Object -First 1

    # RDPGFX_CAPVERSION_* ([MS-RDPEGFX] 2.2.3), same values as ironrdp_egfx::pdu::CapabilityVersion
    $gfxVersions = @{
        0x80004 = '8.0'; 0x80105 = '8.1'; 0xA0002 = '10.0'; 0xA0100 = '10.1'; 0xA0200 = '10.2'
        0xA0301 = '10.3'; 0xA0400 = '10.4'; 0xA0502 = '10.5'; 0xA0600 = '10.6'; 0xA0601 = '10.6 (errata)'
        0xA0701 = '10.7'
    }
    if ($last162) {
        if ($last162.Message -match '0x([0-9A-Fa-f]{5,8})') {
            $version = [Convert]::ToInt32($Matches[1], 16)
            $versionName = if ($gfxVersions.ContainsKey($version)) { "RDPGFX $($gfxVersions[$version])" } else { 'unknown version' }
            Write-Finding 'Graphics pipeline (client caps)' ('0x{0:X5} = {1}' -f $version, $versionName)
        }
        if ($last162.Message -match '(?:Profile|profil)\D{0,5}(\d+)') {
            Write-Finding 'Initial profile (raw)' $Matches[1]
        }
    }

    $codecVerdict = 'Unknown - no event 162 in the time window (reconnect, then re-run)'
    $codecColor = 'Yellow'
    if ($last162) {
        $text = Get-EventText $last162
        if ($text -match 'Hevc') {
            $codecVerdict = 'H.265/HEVC (full-screen video encoding)'
            $codecColor = 'Green'
        }
        elseif ($text -match 'Avc444FullScreen') {
            $codecVerdict = 'H.264/AVC 4:4:4 (full-screen video encoding)'
            $codecColor = 'Green'
        }
        elseif ($text -match 'Avc444|AVC ?444|Initial Profile:\s*2048') {
            $codecVerdict = 'H.264/AVC 4:4:4 (mixed mode)'
            $codecColor = 'Green'
        }
        elseif ($text -match 'AVC\D{0,20}:\s*0\b') {
            $codecVerdict = 'No H.264/AVC (RemoteFX progressive / ClearCodec / planar only)'
            $codecColor = 'Yellow'
        }
        elseif ($text -match 'Avc420|AVC ?420|AVC\D{0,20}:\s*1\b') {
            $codecVerdict = 'H.264/AVC 4:2:0 available (mixed mode: AVC for video-like regions, RemoteFX progressive/ClearCodec for text and UI)'
            $codecColor = 'Green'
        }
        else {
            $codecVerdict = 'No AVC/HEVC profile recognized - see raw event below (likely RemoteFX progressive/ClearCodec only)'
        }
    }
    Write-Finding 'Graphics codec (last connection)' $codecVerdict $codecColor

    $hwVerdict = 'No event 170 - most likely software (CPU) encoding'
    $hwColor = 'Yellow'
    if ($last170) {
        $flag = $null
        if ($last170.Message -match ':\s*(\d+)\s*\.?$') {
            $flag = [int] $Matches[1]
        }
        elseif ($last170.Properties.Count -gt 0) {
            $flag = $last170.Properties[0]
        }
        if ($flag -eq 1 -or $flag -eq $true) {
            $hwVerdict = 'Enabled (GPU encoding)'
            $hwColor = 'Green'
        }
        else {
            $hwVerdict = 'Disabled (software/CPU encoding)'
        }
    }
    Write-Finding 'AVC hardware encoder' $hwVerdict $hwColor
    Write-Host ''
    Write-EventList -Records $codecEvents -Max 6

    # --- Transport events --------------------------------------------------------------------
    Write-Section 'Transport (RdpCoreTS events 131 / 135 and other UDP/TCP related events)'
    $coreEvents = @(Get-RecentEvents -LogName $rdpCoreLog | ForEach-Object { ConvertTo-EventRecord $_ })
    $transportEvents = @($coreEvents | Where-Object {
            $_.Id -in @(131, 135) -or (Get-EventText $_) -match 'UDP|multi.?transport|Multitransport'
        })
    $serverReport['TransportEvents'] = $transportEvents

    # Event 135 is logged once per multitransport tunnel (reliable and lossy UDP each get one).
    # Only the burst belonging to the most recent connection is summarized, and per tunnel the
    # newest outcome wins (events are ordered newest first).
    $tunnelEvents = @($transportEvents | Where-Object Id -EQ 135)
    $tunnels = [ordered]@{}
    if ($tunnelEvents.Count -gt 0) {
        $burstStart = $tunnelEvents[0].Time.AddSeconds(-60)
        foreach ($record in ($tunnelEvents | Where-Object { $_.Time -ge $burstStart })) {
            $tunnel = if ($record.Message -match '[Tt]unnel\D{0,5}(\d+)') { $Matches[1] } else { '?' }
            if ($tunnels.Contains($tunnel)) {
                continue
            }
            $tunnels[$tunnel] = if ($record.Message -match 'TCP:?\s*Reason Code:\s*(\d+)\s*\(([^)]*)\)') {
                "TCP (reason $($Matches[1]): $($Matches[2]))"
            }
            elseif ($record.Message -match 'UDP') {
                'UDP'
            }
            else {
                'TCP'
            }
        }
    }
    $serverReport['Tunnels'] = $tunnels

    if ($tunnels.Count -gt 0) {
        $anyUdp = @($tunnels.Values | Where-Object { $_ -eq 'UDP' }).Count -gt 0
        Write-Finding 'Transport (last connection)' $(if ($anyUdp) { 'UDP (multitransport established)' } else { 'TCP only' }) $(if ($anyUdp) { 'Green' } else { 'Yellow' })
        foreach ($tunnel in $tunnels.Keys) {
            Write-Finding "  Tunnel $tunnel" $tunnels[$tunnel] $(if ($tunnels[$tunnel] -eq 'UDP') { 'Green' } else { 'Gray' })
        }
    }
    elseif ($transportEvents.Count -gt 0) {
        Write-Finding 'Transport (last connection)' 'TCP (no multitransport event found in the time window)' 'Yellow'
    }
    else {
        Write-Finding 'Transport (last connection)' 'Unknown - no transport events in the time window' 'Yellow'
    }
    Write-Host ''
    Write-EventList -Records $transportEvents -Max 10

    # --- Live counters -----------------------------------------------------------------------
    if ($SampleSeconds -gt 0) {
        Write-Section "Live RemoteFX counters ($SampleSeconds samples, generate screen activity now!)"
        $graphics = @(Measure-PerfClass -ClassName 'Win32_PerfFormattedData_Counters_RemoteFXGraphics' -Samples $SampleSeconds)
        $network = @(Measure-PerfClass -ClassName 'Win32_PerfFormattedData_Counters_RemoteFXNetwork' -Samples ([Math]::Min($SampleSeconds, 3)))
        $serverReport['RemoteFXGraphics'] = $graphics
        $serverReport['RemoteFXNetwork'] = $network

        foreach ($instance in @($graphics | Select-Object -ExpandProperty Instance -Unique)) {
            Write-Host ''
            Write-Host "  Instance: $instance" -ForegroundColor Cyan
            $inFps = Get-CounterValue $graphics $instance 'InputFrames*'
            $outFps = Get-CounterValue $graphics $instance 'OutputFrames*'
            $quality = Get-CounterValue $graphics $instance 'FrameQuality*'
            $encode = Get-CounterValue $graphics $instance '*EncodingTime*'
            $ratio = Get-CounterValue $graphics $instance '*Compression*'
            Write-Finding 'Input / output frames per second (avg)' ("{0} / {1}" -f $inFps, $outFps)
            Write-Finding 'Frame quality (avg %, 100 = lossless)' $quality $(if ($null -ne $quality -and $quality -lt 80) { 'Yellow' } else { 'White' })
            Write-Finding 'Average encoding time (ms, < 33 ok)' $encode $(if ($null -ne $encode -and $encode -ge 33) { 'Yellow' } else { 'White' })
            Write-Finding 'Graphics compression ratio' $ratio
            foreach ($reason in 'Client', 'Network', 'Server') {
                $max = Get-CounterValue $graphics $instance "*Skipped*${reason}*" 'Max'
                Write-Finding "Frames skipped/s ($reason resources, max)" $max $(if ($max -gt 0) { 'Yellow' } else { 'White' })
            }
        }

        foreach ($instance in @($network | Select-Object -ExpandProperty Instance -Unique)) {
            Write-Host ''
            Write-Host "  Network instance: $instance" -ForegroundColor Cyan
            $udpRtt = Get-CounterValue $network $instance 'CurrentUDPRTT*' 'Last'
            $tcpRtt = Get-CounterValue $network $instance 'CurrentTCPRTT*' 'Last'
            $udpBw = Get-CounterValue $network $instance 'CurrentUDPBandwidth*' 'Last'
            $tcpBw = Get-CounterValue $network $instance 'CurrentTCPBandwidth*' 'Last'
            $udpRate = Get-CounterValue $network $instance 'UDP*Sent*' 'Max'
            Write-Finding 'Current TCP / UDP RTT (ms)' ("{0} / {1}" -f $tcpRtt, $udpRtt)
            Write-Finding 'Current TCP / UDP bandwidth (Kbps)' ("{0} / {1}" -f $tcpBw, $udpBw)
            Write-Finding 'Loss rate / retransmission rate (%)' ("{0} / {1}" -f (Get-CounterValue $network $instance 'LossRate*' 'Last'), (Get-CounterValue $network $instance 'Retransmission*' 'Last'))
            if ($udpRtt -gt 0 -or $udpBw -gt 0 -or $udpRate -gt 0) {
                Write-Finding 'Active transport (counters)' 'UDP' 'Green'
            }
            else {
                Write-Finding 'Active transport (counters)' 'TCP (no UDP activity measured)' 'Yellow'
            }
        }
        if ($ShowAllCounters) {
            Write-Host ''
            ($graphics + $network) | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
        }
        if ($graphics.Count -eq 0 -and $network.Count -eq 0) {
            Write-Host '  No RemoteFX counter instances found: no active remote session on this host.' -ForegroundColor Yellow
        }
    }

    # --- Configuration -----------------------------------------------------------------------
    Write-Section 'Graphics and transport policy (server)'
    $policies = Get-RegistryValues $policyPath
    $serverReport['Policies'] = $policies
    $knownPolicies = [ordered]@{
        'AVC444ModePreferred'              = 'Prioritize H.264/AVC 444 graphics mode'
        'AVCHardwareEncodePreferred'       = 'H.264/AVC hardware encoding'
        'HEVCHardwareEncodePreferred'      = 'H.265/HEVC hardware encoding (Azure Virtual Desktop ADMX)'
        'bEnumerateHWBeforeSW'             = 'Use hardware graphics adapters'
        'fEnableWddmDriver'                = 'Use WDDM graphics display driver'
        'fEnableVirtualizedGraphics'       = 'Legacy RemoteFX (2008 R2 SP1) encoding'
        'fEnableRemoteFXAdvancedRemoteApp' = 'Advanced RemoteFX graphics for RemoteApp'
        'SelectTransport'                  = 'RDP transport protocols (0 = UDP+TCP, 1 = TCP only, 2 = UDP or TCP)'
        'ColorDepth'                       = 'Limit maximum color depth'
    }
    foreach ($name in $knownPolicies.Keys) {
        $value = if ($policies.Contains($name)) { $policies[$name] } else { 'not configured (default)' }
        Write-Finding $name $value
        Write-Host "      $($knownPolicies[$name])" -ForegroundColor DarkGray
    }
    $otherPolicies = @($policies.Keys | Where-Object { -not $knownPolicies.Contains($_) })
    if ($otherPolicies.Count -gt 0) {
        Write-Host ''
        Write-Host '  Other configured Terminal Services policies:' -ForegroundColor Gray
        foreach ($name in $otherPolicies) {
            Write-Finding $name $policies[$name]
        }
    }

    Write-Host ''
    $frameInterval = (Get-RegistryValues (Join-Path $terminalServerPath 'WinStations'))['DWMFRAMEINTERVAL']
    Write-Finding 'DWMFRAMEINTERVAL (frame rate limit)' $(
        if ($null -ne $frameInterval) { "$frameInterval (~$([Math]::Round(1000 / $frameInterval)) fps max)" } else { 'not set (default ~30 fps)' }
    )

    $rdpTcp = Get-RegistryValues $rdpTcpPath
    $serverReport['RdpTcp'] = $rdpTcp
    $securityLayer = switch ($rdpTcp['SecurityLayer']) { 0 { 'RDP (legacy)' } 1 { 'Negotiate' } 2 { 'TLS' } default { $_ } }
    Write-Finding 'Listener port' $rdpTcp['PortNumber']
    Write-Finding 'Security layer' $securityLayer
    Write-Finding 'NLA required (UserAuthentication)' $rdpTcp['UserAuthentication']

    # --- Sockets / GPU -----------------------------------------------------------------------
    Write-Section 'Sockets and display adapters'
    $port = if ($rdpTcp['PortNumber']) { [int] $rdpTcp['PortNumber'] } else { 3389 }
    try {
        $tcp = @(Get-NetTCPConnection -LocalPort $port -State Established -ErrorAction SilentlyContinue)
        $udp = @(Get-NetUDPEndpoint -LocalPort $port -ErrorAction SilentlyContinue)
        $serverReport['Sockets'] = [ordered]@{
            Tcp = @($tcp | ForEach-Object { "$($_.RemoteAddress):$($_.RemotePort)" })
            UdpListeners = @($udp | ForEach-Object { "$($_.LocalAddress):$($_.LocalPort)" })
        }
        Write-Finding "Established TCP on :$port" (($serverReport['Sockets'].Tcp) -join ', ')
        Write-Finding "UDP endpoints on :$port" (($serverReport['Sockets'].UdpListeners) -join ', ')
    }
    catch {
        Write-Warning "Could not enumerate sockets: $($_.Exception.Message)"
    }
    $adapters = @(Get-CimInstance -ClassName Win32_VideoController | Select-Object Name, DriverVersion, CurrentHorizontalResolution, CurrentVerticalResolution)
    $serverReport['DisplayAdapters'] = $adapters
    foreach ($adapter in $adapters) {
        Write-Finding 'Display adapter' ("{0} (driver {1})" -f $adapter.Name, $adapter.DriverVersion)
    }

    $report['Server'] = $serverReport
}

if ($runClient) {
    $clientReport = [ordered]@{}

    Write-Section 'RDP client (mstsc) events and policy'
    $clientEvents = @(Get-RecentEvents -LogName $rdpClientLog | ForEach-Object { ConvertTo-EventRecord $_ })
    $clientReport['Events'] = $clientEvents
    $interesting = @($clientEvents | Where-Object {
            (Get-EventText $_) -match 'UDP|TCP|transport|Transport|AVC|H\.264|HEVC|codec|Codec'
        })

    if ($clientEvents.Count -eq 0) {
        Write-Host '  No RDP client events in the time window (this machine was not used as a client).' -ForegroundColor DarkGray
    }
    else {
        $udpClient = $interesting | Where-Object { (Get-EventText $_) -match 'UDP' } | Select-Object -First 1
        Write-Finding 'UDP seen in client log' $(if ($udpClient) { "yes (event $($udpClient.Id) at $($udpClient.Time))" } else { 'no' }) $(if ($udpClient) { 'Green' } else { 'Yellow' })
        Write-Host ''
        Write-EventList -Records $interesting -Max 12
    }

    $clientPolicies = Get-RegistryValues $clientPolicyPath
    $clientReport['Policies'] = $clientPolicies
    Write-Host ''
    Write-Finding 'fClientDisableUDP (policy)' $(if ($clientPolicies.Contains('fClientDisableUDP')) { $clientPolicies['fClientDisableUDP'] } else { 'not configured (UDP allowed)' })

    $report['Client'] = $clientReport
}

# --- HEVC / H.265 ---------------------------------------------------------------------------
Write-Section 'HEVC / H.265'
$hevcReport = [ordered]@{}

# Registry: the known policy plus any HEVC/H.265-named value in the RDP related keys, so renamed or
# undocumented switches show up as well
$hevcKeys = @(
    $policyPath,
    $clientPolicyPath,
    $terminalServerPath,
    (Join-Path $terminalServerPath 'WinStations'),
    $rdpTcpPath
)
$hevcValues = [ordered]@{}
foreach ($key in $hevcKeys) {
    $values = Get-RegistryValues $key
    foreach ($name in $values.Keys) {
        if ($name -match 'HEVC|H\.?265') {
            $hevcValues["$key\$name"] = $values[$name]
        }
    }
}
$hevcReport['RegistryValues'] = $hevcValues
$hevcPolicy = (Get-RegistryValues $policyPath)['HEVCHardwareEncodePreferred']
Write-Finding 'HEVCHardwareEncodePreferred (policy)' $(
    if ($null -eq $hevcPolicy) { 'not configured (HEVC off)' } elseif ($hevcPolicy -eq 1) { '1 = enabled' } else { "$hevcPolicy = disabled" }
) $(if ($hevcPolicy -eq 1) { 'Green' } else { 'White' })
if ($hevcValues.Count -gt 0) {
    foreach ($name in $hevcValues.Keys) {
        Write-Finding ($name -replace '^HKLM:\\', '') $hevcValues[$name]
    }
}
else {
    Write-Finding 'HEVC/H.265 registry values' 'none found'
}

# Codec components: the Store HEVC decoder (needed on the client) and Media Foundation HEVC
# transforms (a hardware HEVC encoder MFT on the host is what RDP HEVC encoding needs)
$hevcPackages = $null
try {
    $hevcPackages = @(Get-AppxPackage -Name 'Microsoft.HEVCVideoExtension*' -AllUsers:$isAdmin -ErrorAction Stop |
            Select-Object -Property Name, Version -Unique)
}
catch {
    Write-Host "  Could not query Appx packages: $($_.Exception.Message)" -ForegroundColor DarkGray
}
$hevcReport['Packages'] = $hevcPackages
Write-Finding 'HEVC Video Extension (decoder)' $(
    if ($null -eq $hevcPackages) { 'unknown (package query failed)' }
    elseif ($hevcPackages.Count -gt 0) { ($hevcPackages | ForEach-Object { "$($_.Name) $($_.Version)" }) -join ', ' }
    else { 'not installed' }
) $(if ($null -ne $hevcPackages -and $hevcPackages.Count -gt 0) { 'Green' } else { 'Yellow' })

$hevcTransforms = @()
$transformsKey = 'HKLM:\SOFTWARE\Classes\MediaFoundation\Transforms'
foreach ($transform in @(Get-ChildItem -LiteralPath $transformsKey -ErrorAction SilentlyContinue)) {
    $friendlyName = $transform.GetValue('')
    if ($friendlyName -match 'HEVC|H\.?265') {
        $hevcTransforms += $friendlyName
    }
}
$hevcReport['MediaFoundationTransforms'] = $hevcTransforms
if ($hevcTransforms.Count -gt 0) {
    foreach ($name in $hevcTransforms) {
        Write-Finding 'Media Foundation HEVC transform' $name
    }
}
else {
    Write-Finding 'Media Foundation HEVC transform' 'none registered (no hardware HEVC encoder/decoder MFT)'
}

Write-Host ''
Write-Host '  RDP uses HEVC only with: policy enabled + GPU with HEVC encoder on the host + HEVC decoder on the' -ForegroundColor DarkGray
Write-Host '  client + Windows App / Remote Desktop app as client (mstsc.exe is not listed as supported).' -ForegroundColor DarkGray
Write-Host '  In use when event 162 shows "HevcProfile" (see graphics codec section above).' -ForegroundColor DarkGray
$report['Hevc'] = $hevcReport

Write-Section 'Hints'
Write-Host '  - Codec events (162/170) are written once per connection: reconnect before running if they are missing.'
Write-Host '  - Frame rate / quality counters only move while the screen changes: play a video during sampling.'
Write-Host '  - The mstsc "Connection information" dialog shows transport, RTT and bandwidth from the client side.'

if ($OutputDirectory -or $Export) {
    if (-not $OutputDirectory) {
        $OutputDirectory = Join-Path (Get-SessionUserDesktop) (
            'RdpDiagnostics_{0}_{1:yyyyMMdd_HHmmss}' -f $env:COMPUTERNAME, (Get-Date))
    }
    $null = New-Item -Path $OutputDirectory -ItemType Directory -Force
    $OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).ProviderPath

    # UTF-8 with BOM so Notepad and Excel detect umlauts in localized event messages on every PowerShell version
    $utf8Bom = New-Object System.Text.UTF8Encoding $true
    $writeLines = {
        param([string] $Name, [string[]] $Lines)
        [System.IO.File]::WriteAllLines((Join-Path $OutputDirectory $Name), $Lines, $utf8Bom)
    }

    # Values are pre-formatted with the current culture: PowerShell 7 writes invariant numbers ("99.5"),
    # which e.g. a German Excel would not parse as a number
    $culture = [System.Globalization.CultureInfo]::CurrentCulture
    $eventRows = @()
    $eventSources = @(
        @('Server', 'CodecEvents', 'Codec'),
        @('Server', 'TransportEvents', 'Transport'),
        @('Client', 'Events', 'Client')
    )
    foreach ($source in $eventSources) {
        if ($report.Contains($source[0]) -and $report[$source[0]].Contains($source[1])) {
            $eventRows += @($report[$source[0]][$source[1]] | ForEach-Object {
                    [pscustomobject]@{ Category = $source[2]; Time = $_.Time.ToString('yyyy-MM-dd HH:mm:ss'); Id = $_.Id; Message = $_.Message }
                })
        }
    }

    $counterRows = @()
    foreach ($className in 'RemoteFXGraphics', 'RemoteFXNetwork') {
        if ($report.Contains('Server') -and $report['Server'].Contains($className)) {
            $counterRows += @($report['Server'][$className] | ForEach-Object {
                    [pscustomobject]@{
                        Class    = $className
                        Instance = $_.Instance
                        Counter  = $_.Counter
                        Avg      = ([double] $_.Avg).ToString($culture)
                        Min      = ([double] $_.Min).ToString($culture)
                        Max      = ([double] $_.Max).ToString($culture)
                        Last     = ([double] $_.Last).ToString($culture)
                    }
                })
        }
    }

    & $writeLines 'Report.txt' $reportLines.ToArray()
    & $writeLines 'Report.json' @($report | ConvertTo-Json -Depth 8)
    & $writeLines 'Events.csv' @($eventRows | ConvertTo-Csv -NoTypeInformation -UseCulture)
    & $writeLines 'Counters.csv' @($counterRows | ConvertTo-Csv -NoTypeInformation -UseCulture)

    $zipPath = "$OutputDirectory.zip"
    Compress-Archive -Path (Join-Path $OutputDirectory '*') -DestinationPath $zipPath -Force

    Write-Host ''
    Write-Host "Report exported to $OutputDirectory" -ForegroundColor Green
    Write-Host "Archive for sharing: $zipPath" -ForegroundColor Green
}
