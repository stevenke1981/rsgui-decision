#requires -Version 5.1
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string[]]$Executable,[switch]$LiveScreensaver)
$ErrorActionPreference='Stop'
function Assert($Condition,[string]$Message) {if(-not $Condition){throw $Message}}
function StartMcp([string]$Path,[string]$DecisionMode='local') {
    $start=New-Object Diagnostics.ProcessStartInfo
    $start.FileName=$Path
    $start.Arguments='mcp'
    $start.UseShellExecute=$false
    $start.CreateNoWindow=$true
    $start.RedirectStandardInput=$true
    $start.RedirectStandardOutput=$true
    $start.RedirectStandardError=$true
    $start.StandardOutputEncoding=[Text.Encoding]::UTF8
    $start.StandardErrorEncoding=[Text.Encoding]::UTF8
    if ([string]::IsNullOrEmpty($DecisionMode)) {
        if ($start.EnvironmentVariables.ContainsKey('RSGUI_DECISION_MODE')) { [void]$start.EnvironmentVariables.Remove('RSGUI_DECISION_MODE') }
    } else {
        $start.EnvironmentVariables['RSGUI_DECISION_MODE']=$DecisionMode
    }
    $process=New-Object Diagnostics.Process
    $process.StartInfo=$start
    $inputEncoding=[Console]::InputEncoding
    try { [Console]::InputEncoding=New-Object Text.UTF8Encoding($false); [void]$process.Start() }
    finally { [Console]::InputEncoding=$inputEncoding }
    $stderr=$process.StandardError.ReadToEndAsync()
    @{Process=$process;ErrorTask=$stderr;Next=0}
}
function Send($Client,$Message) { $Client.Process.StandardInput.WriteLine(($Message | ConvertTo-Json -Depth 15 -Compress));$Client.Process.StandardInput.Flush() }
function Receive($Client) {
    $read=$Client.Process.StandardOutput.ReadLineAsync()
    Assert ($read.Wait(15000)) 'MCP response timeout'
    $line=$read.Result
    Assert ($null -ne $line) 'MCP closed unexpectedly'
    $line | ConvertFrom-Json
}
function Request($Client,[string]$Method,$Params) {
    $Client.Next++
    Send $Client @{jsonrpc='2.0';id=$Client.Next;method=$Method;params=$Params}
    $response=Receive $Client
    Assert ($response.id -eq $Client.Next) 'Unexpected response id (stdout contamination?)'
    $response
}
function Initialize($Client) {
    $response=Request $Client initialize @{protocolVersion='2025-11-25';capabilities=@{};clientInfo=@{name='isolated-fixture';version='1'}}
    Assert ($response.result.protocolVersion -eq '2025-11-25') 'Initialization version wrong'
    Send $Client @{jsonrpc='2.0';method='notifications/initialized'}
}
function StopMcp($Client) {
    if($null -eq $Client){return}
    try {
        if(-not $Client.Process.HasExited){$Client.Process.StandardInput.Close(); if(-not $Client.Process.WaitForExit(10000)){$Client.Process.Kill();$Client.Process.WaitForExit();throw 'Server failed EOF shutdown'}}
        Assert ($Client.Process.ExitCode -eq 0) "MCP failed: $($Client.ErrorTask.Result)"
    } finally { $Client.Process.Dispose() }
}
function Loaded($Client) { $Client.Process.Refresh(); @($Client.Process.Modules | Where-Object {$_.ModuleName -eq 'autogui_runtime.dll'}).Count -gt 0 }
function Tool($Client,[string]$Name,$Arguments) {
    $response=Request $Client 'tools/call' @{name=$Name;arguments=$Arguments}
    Assert ($null -eq $response.error) "Protocol error: $($response.error)"
    $response.result
}
function FullPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { [IO.Path]::GetFullPath($Path) }
    else { [IO.Path]::GetFullPath((Join-Path (Get-Location -PSProvider FileSystem).ProviderPath $Path)) }
}
$agentRoot=Join-Path (Split-Path -Parent $PSScriptRoot) '.agent'
foreach ($targetExe in $Executable) {
    $exePath=FullPath $targetExe
    Assert (Test-Path -LiteralPath $exePath -PathType Leaf) "Executable not found: $exePath"
    $client=StartMcp $exePath
    try {
        Assert (-not (Loaded $client)) "DLL loaded on launch: $exePath"
        $before=Request $client 'tools/list' @{}
        Assert ($before.error.code -eq -32002) 'Pre-initialize request should fail'
        Initialize $client
        $list=Request $client 'tools/list' @{}
        $toolNames=@($list.result.tools.name)
        Assert ($list.result.tools.Count -eq 19 -and @($toolNames | Sort-Object -Unique).Count -eq 19) "Wrong tool count for $exePath"
        foreach ($expectedDecision in @('decision_observe','decision_evaluate','decision_act','decision_step','decision_status')) {
            Assert ($toolNames -contains $expectedDecision) "Missing tool $expectedDecision in $exePath"
        }
        Assert (-not (Loaded $client)) 'DLL loaded just to initialize/list tools'
        $client.Process.StandardInput.WriteLine('{')
        $client.Process.StandardInput.Flush()
        Assert ((Receive $client).error.code -eq -32700) 'Malformed JSON not rejected'
        Send $client @{jsonrpc='2.0';method='notifications/unknown'}
        Assert ($null -ne (Request $client ping @{}).result) 'Notification polluted responses'
        Assert ((Request $client 'tools/call' @{name='shell';arguments=@{}}).error.code -eq -32602) 'Unknown tool accepted'
        $invalid=Tool $client type_text @{target='never-existed';text='unused';extra=$true}
        Assert $invalid.isError 'Unknown fields accepted'
        Assert (Loaded $client) 'First tool call did not load runtime DLL'
        foreach ($name in @('click_at','scroll')) {
            $arguments=@{target='never-existed';capture_id='never-existed';x=1;y=1}
            if($name -eq 'scroll'){$arguments.clicks=-3}
            $withoutCapture=Tool $client $name $arguments
            Assert ($withoutCapture.isError -and $withoutCapture.content[0].text -match 'capture') 'Pointer input accepted without a capture proof'
        }
        $decisionStatus=Tool $client decision_status @{}
        Assert (-not $decisionStatus.isError) "decision_status failed: $($decisionStatus.content[0].text)"
        $ds=$decisionStatus.content[0].text | ConvertFrom-Json
        Assert ($ds.mode -eq 'local' -and $ds.evaluations -eq 0 -and $ds.action_attempts -eq 0 -and $null -eq $ds.pending_decision -and $null -eq $ds.provider_model -and $ds.provider_ready -eq $false -and $ds.hook -eq 'prefer_gemini_decision_1' -and $ds.automatic_retries -eq $false) 'Unexpected decision_status payload'
        $badStatus=Tool $client decision_status @{extra=$true}
        Assert $badStatus.isError 'decision_status accepted unknown field'
        $obsMissing=Tool $client decision_observe @{target='never-existed'}
        Assert ($obsMissing.isError -and $obsMissing.content[0].text -match 'target') "decision_observe error unclear: $($obsMissing.content[0].text)"
        $evalBadField=Tool $client decision_evaluate @{target='never-existed';capture_id='never-existed';goal='test';candidates=@();extra=$true}
        Assert $evalBadField.isError 'decision_evaluate accepted unknown field'
        $candidate=@{id='c1';description='test';action=@{type='click';x=1;y=1}}
        $evalNoCapture=Tool $client decision_evaluate @{target='never-existed';capture_id='never-existed';goal='test';candidates=@($candidate)}
        Assert ($evalNoCapture.isError -and $evalNoCapture.content[0].text -match 'capture|observe') "decision_evaluate error unclear: $($evalNoCapture.content[0].text)"
        $actNoPending=Tool $client decision_act @{decision_id='never-existed'}
        Assert ($actNoPending.isError -and $actNoPending.content[0].text -match 'no unused decision|evaluate') "decision_act error unclear: $($actNoPending.content[0].text)"
        $stepBadField=Tool $client decision_step @{request=@{target='never-existed';capture_id='never-existed';goal='test';candidates=@($candidate)};extra=$true}
        Assert $stepBadField.isError 'decision_step accepted unknown field'
        $stepNoCapture=Tool $client decision_step @{request=@{target='never-existed';capture_id='never-existed';goal='test';candidates=@($candidate)}}
        Assert ($stepNoCapture.isError -and $stepNoCapture.content[0].text -match 'capture|observe') "decision_step error unclear: $($stepNoCapture.content[0].text)"
        $status=Tool $client screensaver @{action='status'}
        Assert (-not $status.isError) 'Screensaver status failed through DLL'
        if($LiveScreensaver) {
            $initial=$status.content[0].text | ConvertFrom-Json
            $pause=Tool $client screensaver @{action='pause';seconds=20}
            Assert (-not $pause.isError) "Managed pause failed: $($pause.content[0].text)"
            $during=Tool $client screensaver @{action='status'}
            $duringText=($during.content[0].text | ConvertFrom-Json).status_text
            Assert ($duringText -match '螢幕保護程式啟用：false') 'Pause did not disable auto-start'
            Assert ($duringText -match '本工具暫停：執行中') 'Managed pause is not active'
            StopMcp $client
            $client=StartMcp $exePath
            Initialize $client
            $after=Tool $client screensaver @{action='status'}
            $afterText=($after.content[0].text | ConvertFrom-Json).status_text
            Assert ($afterText -ceq $initial.status_text) 'MCP EOF did not restore exact screensaver status'
        }
    } finally {StopMcp $client}

    # Verify default mode is auto when RSGUI_DECISION_MODE is unset and PreferGeminiDecisionHook is active.
    $defaultClient=$null
    try {
        $defaultClient=StartMcp $exePath ''
        Initialize $defaultClient
        $defaultStatus=Tool $defaultClient decision_status @{}
        Assert (-not $defaultStatus.isError) "Default auto decision_status failed: $($defaultStatus.content[0].text)"
        $dsAuto=$defaultStatus.content[0].text | ConvertFrom-Json
        Assert ($dsAuto.mode -eq 'auto' -and $dsAuto.provider_model -eq 'gemini-decision-1' -and $dsAuto.hook -eq 'prefer_gemini_decision_1' -and ($null -ne $dsAuto.gemini_decision_1)) "Unexpected default decision_status payload: $($defaultStatus.content[0].text)"
    } finally {StopMcp $defaultClient}

    # A separated EXE remains able to describe tools and reports missing DLL clearly.
    $agentRootExisted=Test-Path -LiteralPath $agentRoot
    $fixture=Join-Path $agentRoot ('missing-dll-'+[guid]::NewGuid().ToString('N'))
    $client=$null
    try {
        New-Item -ItemType Directory -Path $fixture | Out-Null
        $separated=Join-Path $fixture (Split-Path -Leaf $exePath)
        Copy-Item -LiteralPath $exePath -Destination $separated
        $client=StartMcp $separated
        Initialize $client
        Assert ((Request $client 'tools/list' @{}).result.tools.Count -eq 19) 'Tool listing requires DLL'
        foreach ($call in @(@{Name='screensaver';Args=@{action='status'}},@{Name='decision_status';Args=@{}})) {
            $missing=Tool $client $call.Name $call.Args
            Assert ($missing.isError -and $missing.content[0].text -match 'autogui_runtime\.dll') "Missing DLL failure unclear for $($call.Name)"
        }
    } finally {
        StopMcp $client
        if(Test-Path -LiteralPath $fixture){Remove-Item -LiteralPath $fixture -Recurse -Force -ErrorAction SilentlyContinue}
        if(-not $agentRootExisted -and (Test-Path -LiteralPath $agentRoot) -and @((Get-ChildItem -LiteralPath $agentRoot -Force)).Count -eq 0){
            Remove-Item -LiteralPath $agentRoot -Force -ErrorAction SilentlyContinue
        }
    }
}
Write-Host "PASS MCP: lazy module list verified for $($Executable.Count) executable(s), JSON-RPC errors/notifications, tool schema, decision_* validation, DLL invocation, missing-DLL diagnostics; live screensaver=$LiveScreensaver"
