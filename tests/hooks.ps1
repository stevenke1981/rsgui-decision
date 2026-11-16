#requires -Version 5.1
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'

function Assert($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$repo = Split-Path -Parent $PSScriptRoot
$agentsDir = Join-Path $repo '.agents'
$hooksJsonPath = Join-Path $agentsDir 'hooks.json'
$hookScriptPath = Join-Path $agentsDir 'hooks\prefer-gemini-decision.ps1'

Assert (Test-Path -LiteralPath $hooksJsonPath -PathType Leaf) 'Missing .agents/hooks.json'
Assert (Test-Path -LiteralPath $hookScriptPath -PathType Leaf) 'Missing .agents/hooks/prefer-gemini-decision.ps1'

$hooksConfig = Get-Content -LiteralPath $hooksJsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
$entry = $hooksConfig.'prefer-gemini-decision-1'
Assert ($null -ne $entry) 'Missing prefer-gemini-decision-1 hook entry'
Assert ($entry.SessionStart.Count -ge 1 -and $entry.PreToolUse.Count -ge 1) 'Hook events missing'

$isolatedPath = "$env:SystemRoot\System32\WindowsPowerShell\v1.0;$env:SystemRoot\System32;$env:SystemRoot"

function Invoke-Hook([string]$Command, [hashtable]$Payload, [hashtable]$ExtraEnv = @{}) {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = 'cmd.exe'
    $start.Arguments = "/c $Command"
    $start.WorkingDirectory = $agentsDir
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.Encoding]::UTF8
    $start.StandardErrorEncoding = [Text.Encoding]::UTF8

    foreach ($name in @('RSGUI_DECISION_MODE', 'GEMINI_API_KEY', 'GOOGLE_API_KEY', 'GEMINI_CORE_HOME', 'RSGUI_DECISION_PROBE_SIDECAR', 'RSGUI_HOOK_TEST_SIDECAR_PORT')) {
        if ($start.EnvironmentVariables.ContainsKey($name)) {
            [void]$start.EnvironmentVariables.Remove($name)
        }
    }
    $start.EnvironmentVariables['PATH'] = $isolatedPath
    $start.EnvironmentVariables['RSGUI_HOOK_DISABLE_USER_ENV'] = '1'
    $start.EnvironmentVariables['RSGUI_DECISION_PROBE_SIDECAR'] = '0'
    foreach ($key in $ExtraEnv.Keys) {
        if ($null -eq $ExtraEnv[$key]) {
            if ($start.EnvironmentVariables.ContainsKey($key)) { [void]$start.EnvironmentVariables.Remove($key) }
        } else {
            $start.EnvironmentVariables[$key] = [string]$ExtraEnv[$key]
        }
    }

    $process = New-Object Diagnostics.Process
    $process.StartInfo = $start
    $originalInputEncoding = [Console]::InputEncoding
    try {
        [Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
        [void]$process.Start()
    } finally {
        [Console]::InputEncoding = $originalInputEncoding
    }
    try {
        $json = $Payload | ConvertTo-Json -Depth 8 -Compress
        $process.StandardInput.Write($json)
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEnd()
        $stderr = $process.StandardError.ReadToEnd()
        Assert ($process.WaitForExit(10000)) 'Hook timed out'
        Assert ($process.ExitCode -eq 0) "Hook failed with exit $($process.ExitCode): $stderr"
        $stdout.Trim() | ConvertFrom-Json
    } finally {
        $process.Dispose()
    }
}

$sessionCmd = [string]$entry.SessionStart[0].command
$preToolCmd = [string]$entry.PreToolUse[0].hooks[0].command
$matcher = [string]$entry.PreToolUse[0].matcher

Assert ('call_mcp_tool' -match $matcher -and 'mcp_autogui_click_at' -match $matcher -and 'click_at' -match $matcher) 'PreToolUse matcher missed AutoGui tools'
Assert (-not ('run_command' -match "^($matcher)$")) 'PreToolUse matcher unexpectedly matched run_command'

# 1. Absent Gemini Decision 1 -> SessionStart returns empty object {} and never blocks.
$none = Invoke-Hook $sessionCmd @{ hookEventName = 'SessionStart'; conversationId = 'test' }
Assert ($null -eq $none.PSObject.Properties['injectSteps']) 'Unexpected injectSteps when Gemini Decision 1 absent'

# 2. With GEMINI_API_KEY in default auto mode -> injects priority instruction without leaking secret.
$secretMarker = 'fixture-secret-value-never-output'
$withKey = Invoke-Hook $sessionCmd @{ hookEventName = 'SessionStart'; conversationId = 'test' } @{ GEMINI_API_KEY = $secretMarker }
Assert ($withKey.injectSteps.Count -eq 1) 'Expected systemMessage step when API key exists'
$msg = [string]$withKey.injectSteps[0].systemMessage.systemMessage
Assert ($msg -match 'Gemini Decision 1' -and $msg -match 'mode=auto' -and $msg -match 'api_key=True') "Unexpected SessionStart message: $msg"
Assert (-not $msg.Contains($secretMarker)) 'Hook leaked API key value into stdout'

# 3. Explicit RSGUI_DECISION_MODE=local respects user opt-out even if key exists.
$forcedLocal = Invoke-Hook $sessionCmd @{ hookEventName = 'SessionStart'; conversationId = 'test' } @{ RSGUI_DECISION_MODE = 'local'; GEMINI_API_KEY = $secretMarker }
Assert ($null -eq $forcedLocal.PSObject.Properties['injectSteps']) 'Local mode should not inject Gemini priority prompt'

# 4. PreToolUse for raw AutoGui call adds Gemini priority reason; non-autogui or decision_* stays quiet allow.
$preRaw = Invoke-Hook $preToolCmd @{
    hookEventName = 'PreToolUse'
    toolCall = @{ name = 'call_mcp_tool'; args = @{ ServerName = 'autogui'; ToolName = 'click_at' } }
} @{ GEMINI_API_KEY = $secretMarker }
Assert ($preRaw.decision -eq 'allow' -and $preRaw.reason -match 'decision_observe') "Unexpected PreToolUse raw AutoGui result: $($preRaw | ConvertTo-Json -Compress)"
Assert (-not $preRaw.reason.Contains($secretMarker)) 'PreToolUse leaked secret'

foreach ($rawAlias in @('mcp_autogui_click_at', 'click_at')) {
    $aliasOut = Invoke-Hook $preToolCmd @{
        hookEventName = 'PreToolUse'
        toolCall = @{ name = $rawAlias; args = @{} }
    } @{ GEMINI_API_KEY = $secretMarker }
    Assert ($aliasOut.decision -eq 'allow' -and $aliasOut.reason -match 'decision_observe') "PreToolUse failed for $rawAlias"
}

$preInvocation = Invoke-Hook $sessionCmd @{ hookEventName = 'PreInvocation'; conversationId = 'test' } @{ GEMINI_API_KEY = $secretMarker }
Assert ($preInvocation.injectSteps.Count -eq 1) 'PreInvocation handler failed when key exists'

# Isolated temporary gemini-core CLI on PATH detection.
$tempBin = Join-Path ([IO.Path]::GetTempPath()) ('rsgui-hook-bin-' + [guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $tempBin -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $tempBin 'gemini-core.cmd'), "@echo off`r`n", [Text.Encoding]::ASCII)
    $withCli = Invoke-Hook $sessionCmd @{ hookEventName = 'SessionStart'; conversationId = 'test' } @{
        PATH = "$tempBin;$isolatedPath"
    }
    Assert ($withCli.injectSteps.Count -eq 1 -and [string]$withCli.injectSteps[0].systemMessage.systemMessage -match 'cli=True') 'Expected cli=True when gemini-core is on PATH'
} finally {
    Remove-Item -LiteralPath $tempBin -Recurse -Force -ErrorAction SilentlyContinue
}

$preDecision = Invoke-Hook $preToolCmd @{
    hookEventName = 'PreToolUse'
    toolCall = @{ name = 'call_mcp_tool'; args = @{ ServerName = 'autogui'; ToolName = 'decision_step' } }
} @{ GEMINI_API_KEY = $secretMarker }
Assert ($preDecision.decision -eq 'allow' -and $null -eq $preDecision.PSObject.Properties['reason']) 'PreToolUse decision_step should not emit raw-tool warning'

$preOther = Invoke-Hook $preToolCmd @{
    hookEventName = 'PreToolUse'
    toolCall = @{ name = 'call_mcp_tool'; args = @{ ServerName = 'other_mcp'; ToolName = 'click_at' } }
} @{ GEMINI_API_KEY = $secretMarker }
Assert ($preOther.decision -eq 'allow' -and $null -eq $preOther.PSObject.Properties['reason']) 'PreToolUse touched unrelated MCP server'

# 5. Live loopback sidecar HTTP /health fixture detection.
$listener = New-Object System.Net.Sockets.TcpListener([Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
$acceptTask = $listener.AcceptTcpClientAsync()
$readyEvent = New-Object System.Threading.ManualResetEventSlim($false)
try {
    $sidecarRunspace = [PowerShell]::Create()
    $null = $sidecarRunspace.AddScript({
        param($Task, $Ready)
        $Ready.Set()
        $client = $Task.GetAwaiter().GetResult()
        try {
            $stream = $client.GetStream()
            $buf = New-Object byte[] 256
            $null = $stream.Read($buf, 0, $buf.Length)
            $resp = [Text.Encoding]::ASCII.GetBytes("HTTP/1.1 200 OK`r`nContent-Length: 11`r`nConnection: close`r`n`r`n{""ok"":true}")
            $stream.Write($resp, 0, $resp.Length)
            $stream.Flush()
        } finally {
            $client.Dispose()
        }
    }).AddArgument($acceptTask).AddArgument($readyEvent)
    $asyncHandle = $sidecarRunspace.BeginInvoke()
    Assert ($readyEvent.Wait(5000)) 'Sidecar fixture failed to become ready'
    $withSidecar = Invoke-Hook $sessionCmd @{ hookEventName = 'SessionStart'; conversationId = 'test' } @{
        RSGUI_DECISION_PROBE_SIDECAR = '1'
        RSGUI_HOOK_TEST_SIDECAR_PORT = "$port"
    }
    $sidecarRunspace.EndInvoke($asyncHandle)
    $sidecarRunspace.Dispose()
    Assert ($null -ne $withSidecar.PSObject.Properties['injectSteps'] -and $withSidecar.injectSteps.Count -eq 1) "Expected injectSteps when loopback sidecar is healthy, got: $($withSidecar | ConvertTo-Json -Compress)"
    $sidecarMsg = [string]$withSidecar.injectSteps[0].systemMessage.systemMessage
    Assert ($sidecarMsg -match 'sidecar=True') "Expected sidecar=True in message: $sidecarMsg"
} finally {
    $readyEvent.Dispose()
    $listener.Stop()
}

Write-Host 'PASS hooks: .agents/hooks.json schema, SessionStart injection, PreToolUse raw-tool priority, local opt-out, secret isolation, and loopback sidecar probe verified.'
