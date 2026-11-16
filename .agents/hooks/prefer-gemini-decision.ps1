#requires -Version 5.1
Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'

$utf8 = New-Object System.Text.UTF8Encoding($false)
try {
    [Console]::InputEncoding = $utf8
    [Console]::OutputEncoding = $utf8
} catch {
}

function Emit-Json([hashtable]$Object) {
    [Console]::Out.WriteLine(($Object | ConvertTo-Json -Compress -Depth 6))
}

function Read-Setting([string]$Name) {
    $value = [Environment]::GetEnvironmentVariable($Name, 'Process')
    if (-not [string]::IsNullOrWhiteSpace($value)) {
        return $value
    }
    $disableUser = [Environment]::GetEnvironmentVariable('RSGUI_HOOK_DISABLE_USER_ENV', 'Process')
    if ($disableUser -in @('1', 'true', 'yes')) {
        return $null
    }
    $userValue = [Environment]::GetEnvironmentVariable($Name, 'User')
    if (-not [string]::IsNullOrWhiteSpace($userValue)) {
        return $userValue
    }
    return $null
}

function Test-SidecarHealth([int]$Port) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $task = $client.ConnectAsync('127.0.0.1', $Port)
        if (-not $task.Wait(200)) {
            return $false
        }
        $client.ReceiveTimeout = 500
        $client.SendTimeout = 500
        $stream = $client.GetStream()
        $req = [Text.Encoding]::ASCII.GetBytes("GET /health HTTP/1.1`r`nHost: 127.0.0.1`r`nConnection: close`r`n`r`n")
        $stream.Write($req, 0, $req.Length)
        $stream.Flush()
        $buf = New-Object byte[] 256
        $read = $stream.Read($buf, 0, $buf.Length)
        if ($read -le 0) {
            return $false
        }
        $head = [Text.Encoding]::ASCII.GetString($buf, 0, $read)
        return ($head.StartsWith('HTTP/1.1 200') -or $head.StartsWith('HTTP/1.0 200'))
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

function Get-Presence([switch]$IncludeSidecar) {
    $rawMode = Read-Setting 'RSGUI_DECISION_MODE'
    $mode = if ([string]::IsNullOrWhiteSpace($rawMode)) { 'auto' } else { $rawMode.Trim().ToLowerInvariant() }
    $forcedLocal = ($mode -eq 'local')

    $apiKeyConfigured = (-not [string]::IsNullOrWhiteSpace((Read-Setting 'GEMINI_API_KEY'))) -or
                        (-not [string]::IsNullOrWhiteSpace((Read-Setting 'GOOGLE_API_KEY')))

    $disableUser = [Environment]::GetEnvironmentVariable('RSGUI_HOOK_DISABLE_USER_ENV', 'Process') -in @('1', 'true', 'yes')
    $coreHome = Read-Setting 'GEMINI_CORE_HOME'
    $defaultCoreHome = if (-not $disableUser -and -not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        Join-Path $env:USERPROFILE '.gemini-decision-1'
    } else {
        $null
    }
    $geminiCoreCli = ([bool](Get-Command gemini-core -ErrorAction SilentlyContinue)) -or
                     (-not [string]::IsNullOrWhiteSpace($coreHome) -and (Test-Path -LiteralPath $coreHome -PathType Container)) -or
                     (-not [string]::IsNullOrWhiteSpace($defaultCoreHome) -and (Test-Path -LiteralPath $defaultCoreHome -PathType Container))

    $sidecarHealthy = $false
    $probeFlag = Read-Setting 'RSGUI_DECISION_PROBE_SIDECAR'
    $probeEnabled = [string]::IsNullOrWhiteSpace($probeFlag) -or ($probeFlag.Trim().ToLowerInvariant() -notin @('0', 'false', 'off', 'no'))
    if ($IncludeSidecar -and $probeEnabled -and -not $forcedLocal) {
        $port = 8792
        $overridePort = Read-Setting 'RSGUI_HOOK_TEST_SIDECAR_PORT'
        if (-not [string]::IsNullOrWhiteSpace($overridePort)) {
            [int]$parsed = 0
            if ([int]::TryParse($overridePort.Trim(), [ref]$parsed) -and $parsed -ge 1 -and $parsed -le 65535) {
                $port = $parsed
            }
        }
        $sidecarHealthy = Test-SidecarHealth $port
    }

    @{
        Mode = $mode
        ForcedLocal = $forcedLocal
        ApiKeyConfigured = $apiKeyConfigured
        GeminiCoreCli = $geminiCoreCli
        SidecarHealthy = $sidecarHealthy
        Detected = (-not $forcedLocal) -and ($apiKeyConfigured -or $geminiCoreCli -or $sidecarHealthy)
    }
}

$rawInput = [Console]::In.ReadToEnd()
$payload = $null
if (-not [string]::IsNullOrWhiteSpace($rawInput)) {
    try {
        $payload = $rawInput | ConvertFrom-Json
    } catch {
        $payload = $null
    }
}

$eventName = if ($null -ne $payload -and $payload.PSObject.Properties['hookEventName']) { [string]$payload.hookEventName } else { '' }

if ($eventName -eq 'PreToolUse') {
    $toolName = ''
    $toolArgs = $null
    if ($null -ne $payload -and $payload.PSObject.Properties['toolCall'] -and $null -ne $payload.toolCall) {
        if ($payload.toolCall.PSObject.Properties['name']) {
            $toolName = [string]$payload.toolCall.name
        }
        if ($payload.toolCall.PSObject.Properties['args']) {
            $toolArgs = $payload.toolCall.args
        }
    }

    $effectiveTool = $toolName
    if ($toolName -eq 'call_mcp_tool') {
        $serverName = if ($null -ne $toolArgs -and $toolArgs.PSObject.Properties['ServerName']) { [string]$toolArgs.ServerName } else { '' }
        if (-not [string]::Equals($serverName, 'autogui', [StringComparison]::OrdinalIgnoreCase)) {
            Emit-Json @{ decision = 'allow' }
            exit 0
        }
        $effectiveTool = if ($null -ne $toolArgs -and $toolArgs.PSObject.Properties['ToolName']) { [string]$toolArgs.ToolName } else { '' }
    } elseif ($toolName.StartsWith('mcp_autogui_', [StringComparison]::OrdinalIgnoreCase)) {
        $effectiveTool = $toolName.Substring('mcp_autogui_'.Length)
    }

    $rawGuiTools = @('click_at', 'click_image', 'locate', 'scroll', 'type_text', 'hotkey')
    if ($effectiveTool -notin $rawGuiTools) {
        Emit-Json @{ decision = 'allow' }
        exit 0
    }

    $presence = Get-Presence -IncludeSidecar
    if ($presence.Detected) {
        Emit-Json @{
            decision = 'allow'
            reason = "Gemini Decision 1 detected (mode=$($presence.Mode), api_key=$($presence.ApiKeyConfigured), cli=$($presence.GeminiCoreCli), sidecar=$($presence.SidecarHealthy)). Prefer decision_select or decision_observe + decision_evaluate/decision_step with bounded candidates when selecting UI actions."
        }
    } else {
        Emit-Json @{ decision = 'allow' }
    }
    exit 0
}

if ($eventName -eq 'SessionStart' -or $eventName -eq 'PreInvocation') {
    $presence = Get-Presence -IncludeSidecar
    if ($presence.Detected) {
        $message = "Gemini Decision 1 is available (mode=$($presence.Mode), api_key=$($presence.ApiKeyConfigured), cli=$($presence.GeminiCoreCli), sidecar=$($presence.SidecarHealthy)). Prioritize rsgui-decision MCP tools (decision_status, decision_select, decision_observe, decision_evaluate, decision_step) with concrete candidates for visual UI choices so Gemini Decision 1 and fast local template paths optimize web/app selection."
        Emit-Json @{
            injectSteps = @(
                @{
                    systemMessage = @{
                        systemMessage = $message
                    }
                }
            )
        }
    } else {
        Emit-Json @{}
    }
    exit 0
}

Emit-Json @{}
exit 0
