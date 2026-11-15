#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$PackageDir,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\AutoGui'),
    [string]$BinDir = (Join-Path $env:USERPROFILE '.local\bin'),
    # Opt in only when preserving a running image by rename is intended.
    [switch]$AllowInUseRename
)
$ErrorActionPreference = 'Stop'
$utf8 = New-Object Text.UTF8Encoding($false)

function FullPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if ($full.Length -gt [IO.Path]::GetPathRoot($full).Length) { $full = $full.TrimEnd('\') }
    $full
}
function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }
function CheckPath([string]$Path) {
    $cursor = $Path
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse paths are unsupported: $cursor" }
        }
        $parent = Split-Path -Parent $cursor
        if ($parent -eq $cursor) { break }
        $cursor = $parent
    }
}
function CheckUnlocked([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Expected a file: $Path" }
        # Opening exclusively also rejects files mapped by a running executable/DLL.
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        $stream.Dispose()
    }
}
function MoveVerified([string]$From,[string]$To,[string]$ExpectedHash) {
    if ((FullPath $From) -ne $From -or (FullPath $To) -ne $To -or -not $movePairs.ContainsKey($From) -or $movePairs[$From] -ne $To) { throw "Unapproved move: $From -> $To" }
    foreach ($path in @($From,$To)) { CheckPath $path }
    if (-not (Test-Path -LiteralPath $From -PathType Leaf) -or (Test-Path -LiteralPath $To) -or (Hash $From) -ne $ExpectedHash) { throw "Move source/destination changed: $From -> $To" }
    Move-Item -LiteralPath $From -Destination $To
    if ((Hash $To) -ne $ExpectedHash) { throw "Moved checksum mismatch: $To" }
}
function StartProbe([string]$Path, [string]$Arguments) {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $Path
    $start.Arguments = $Arguments
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.Encoding]::UTF8
    $start.StandardErrorEncoding = [Text.Encoding]::UTF8
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $start
    # .NET Framework creates stdin's AutoFlush writer during Start(), inheriting
    # Console.InputEncoding. A UTF-8 BOM would corrupt the first JSONL request.
    $inputEncoding = [Console]::InputEncoding
    try {
        [Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
        [void]$process.Start()
    } finally { [Console]::InputEncoding = $inputEncoding }
    @{ Process=$process; ErrorTask=$process.StandardError.ReadToEndAsync() }
}
function VerifyLoader([string]$Path, [switch]$Mcp) {
    $arguments = if ($Mcp) { 'mcp' } else { '--version' }
    $probe = StartProbe $Path $arguments
    $process = $probe.Process
    try {
        if (-not $Mcp) {
            $output = $process.StandardOutput.ReadToEndAsync()
            $process.StandardInput.Close()
            if (-not $process.WaitForExit(10000)) { throw "Version probe timed out: $Path" }
            $line = $output.Result.Trim()
            if ($process.ExitCode -ne 0 -or $line -notmatch '^[^\r\n]+\s+(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?)$') {
                throw "Invalid --version response from ${Path}: $line $($probe.ErrorTask.Result)"
            }
            return $Matches[1]
        }
        $requests = @(
            @{jsonrpc='2.0';id=1;method='initialize';params=@{protocolVersion='2025-11-25';capabilities=@{};clientInfo=@{name='local-install';version='1'}}},
            @{jsonrpc='2.0';id=2;method='tools/list';params=@{}}
        )
        foreach ($request in $requests) {
            $process.StandardInput.WriteLine(($request | ConvertTo-Json -Depth 8 -Compress))
            $process.StandardInput.Flush()
            $read = $process.StandardOutput.ReadLineAsync()
            if (-not $read.Wait(10000) -or $null -eq $read.Result) { throw "MCP response timed out/closed: $Path" }
            $response = $read.Result | ConvertFrom-Json
            if ($response.id -ne $request.id -or $null -ne $response.error) { throw "MCP request $($request.method) failed: $Path" }
            if ($request.id -eq 1) {
                if ($response.result.protocolVersion -ne '2025-11-25') { throw "Unexpected MCP protocol: $Path" }
                $process.StandardInput.WriteLine('{"jsonrpc":"2.0","method":"notifications/initialized"}')
                $process.StandardInput.Flush()
            } else {
                $tools = @($response.result.tools)
                if ($tools.Count -ne 19 -or @($tools.name | Sort-Object -Unique).Count -ne 19) { throw "Expected 19 unique MCP tools: $Path" }
            }
        }
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(10000)) { throw "MCP EOF shutdown timed out: $Path" }
        if ($process.ExitCode -ne 0) { throw "MCP validation failed: $Path $($probe.ErrorTask.Result)" }
    } finally {
        # Only terminate a validation child started by this invocation.
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
}

$PackageDir = FullPath $PackageDir
$InstallDir = FullPath $InstallDir
$BinDir = FullPath $BinDir
foreach ($root in @($PackageDir,$InstallDir,$BinDir)) {
    CheckPath $root
    if ((Test-Path -LiteralPath $root) -and -not (Test-Path -LiteralPath $root -PathType Container)) { throw "Expected a directory: $root" }
}
$payloadPath = Join-Path $PackageDir 'payload.json'
$sumsPath = Join-Path $PackageDir 'SHA256SUMS.txt'
$payload = [IO.File]::ReadAllText($payloadPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
if ($payload.version -ne 1 -or $null -eq $payload.files) { throw 'Unsupported package payload schema' }
$sums = @{}
foreach ($line in [IO.File]::ReadAllLines($sumsPath, [Text.Encoding]::UTF8)) {
    if ($line -notmatch '^([A-Fa-f0-9]{64})  (.+)$' -or $sums.ContainsKey($Matches[2])) { throw 'Malformed/duplicate SHA256SUMS entry' }
    $sums[$Matches[2]] = $Matches[1]
}
$payloadHash = Hash $payloadPath
if ($sums['payload.json'] -ne $payloadHash) { throw 'payload.json checksum mismatch' }
$packageFiles = @{}
foreach ($file in $payload.files) {
    if ([string]::IsNullOrWhiteSpace($file.path) -or [IO.Path]::IsPathRooted($file.path) -or $file.sha256 -notmatch '^[A-Fa-f0-9]{64}$' -or $file.bytes -isnot [ValueType] -or $file.bytes -lt 0) { throw 'Invalid package file entry' }
    $source = FullPath (Join-Path $PackageDir $file.path)
    if (-not $source.StartsWith($PackageDir + '\', [StringComparison]::OrdinalIgnoreCase) -or $packageFiles.ContainsKey($source)) { throw "Invalid/duplicate payload path: $($file.path)" }
    CheckPath $source
    if (-not (Test-Path -LiteralPath $source -PathType Leaf) -or (Get-Item -LiteralPath $source).Length -ne $file.bytes -or (Hash $source) -ne $file.sha256 -or $sums[$file.path] -ne $file.sha256) { throw "Package checksum/length mismatch: $source" }
    $packageFiles[$source] = $file
}
$mandatory = @('autogui-control.exe','rsgui-decision.exe','autogui_runtime.dll')
foreach ($name in $mandatory) {
    if (-not $packageFiles.ContainsKey((Join-Path $PackageDir $name))) { throw "Missing mandatory package file: $name" }
}
$targets = @(
    @{root=$InstallDir;name='autogui-control.exe'}, @{root=$InstallDir;name='autogui_runtime.dll'},
    @{root=$BinDir;name='rsgui-decision.exe'}, @{root=$BinDir;name='autogui_runtime.dll'}
)
$uniqueTargets = @{}
foreach ($target in $targets) {
    $path = Join-Path $target.root $target.name
    if ($path.StartsWith($PackageDir + '\', [StringComparison]::OrdinalIgnoreCase)) { throw "Package cannot also be an installation target: $path" }
    CheckPath $path
    if (-not $AllowInUseRename) { CheckUnlocked $path }
    elseif ((Test-Path -LiteralPath $path) -and -not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Expected a file: $path" }
    $uniqueTargets[$path] = $target
}

$transactionId = (Get-Date -Format 'yyyyMMdd_HHmmss') + '_' + [guid]::NewGuid().ToString('N')
$transactionRoot = Join-Path $InstallDir ('.local-replacements\' + $transactionId)
CheckPath $transactionRoot
if (Test-Path -LiteralPath $transactionRoot) { throw "Backup directory already exists: $transactionRoot" }
$stageDir = Join-Path $transactionRoot 'stage'
$backupDir = Join-Path $transactionRoot 'backup'
$displacedDir = Join-Path $transactionRoot 'displaced'
$manifestPath = Join-Path $transactionRoot 'manifest.json'
New-Item -ItemType Directory -Path $stageDir | Out-Null
New-Item -ItemType Directory -Path $backupDir | Out-Null
if ($AllowInUseRename) { New-Item -ItemType Directory -Path $displacedDir | Out-Null }
# Probe --version in isolation before placing the runtime beside either loader.
$versions = @()
foreach ($name in @('autogui-control.exe','rsgui-decision.exe')) {
    $staged = Join-Path $stageDir $name
    Copy-Item -LiteralPath (Join-Path $PackageDir $name) -Destination $staged
    if ((Hash $staged) -ne $packageFiles[(Join-Path $PackageDir $name)].sha256) { throw "Staged checksum mismatch: $staged" }
    $versions += VerifyLoader $staged
}
if ($versions[0] -ne $versions[1]) { throw 'Loader versions do not match' }
Copy-Item -LiteralPath (Join-Path $PackageDir 'autogui_runtime.dll') -Destination (Join-Path $stageDir 'autogui_runtime.dll')
if ((Hash (Join-Path $stageDir 'autogui_runtime.dll')) -ne $packageFiles[(Join-Path $PackageDir 'autogui_runtime.dll')].sha256) { throw 'Staged runtime checksum mismatch' }
foreach ($name in @('autogui-control.exe','rsgui-decision.exe')) { VerifyLoader (Join-Path $stageDir $name) -Mcp }

$records = @()
$movePairs = @{}
foreach ($path in @($uniqueTargets.Keys | Sort-Object)) {
    if (-not $AllowInUseRename) { CheckUnlocked $path }
    $target = $uniqueTargets[$path]
    $existed = Test-Path -LiteralPath $path -PathType Leaf
    $oldHash = $null
    $backup = $null
    $displaced = $null
    if ($existed) {
        $oldHash = Hash $path
        $backup = Join-Path $backupDir ('{0:D2}.bin' -f $records.Count)
        Copy-Item -LiteralPath $path -Destination $backup
        if ((Hash $backup) -ne $oldHash) { throw "Backup checksum mismatch: $backup" }
        if ($AllowInUseRename) {
            $displaced = Join-Path $displacedDir ('{0:D2}.bin' -f $records.Count)
            $movePairs[$path] = $displaced
            $movePairs[$displaced] = $path
        }
    }
    $records += [ordered]@{path=$path;source=$target.name;existed=$existed;old_sha256=$oldHash;new_sha256=$packageFiles[(Join-Path $PackageDir $target.name)].sha256;backup=$backup;displaced=$displaced}
}
$manifest = [ordered]@{
    schema=1;transaction_id=$transactionId;created_at=(Get-Date -Format 'o');state='installing'
    install_dir=$InstallDir;bin_dir=$BinDir;package_dir=$PackageDir;payload_sha256=$payloadHash;loader_version=$versions[0]
    integrity='Unsigned package; SHA256 verifies consistency, not publisher identity.';files=$records
    allow_in_use_rename=[bool]$AllowInUseRename
}
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), $utf8)
$attempted = @()
try {
    foreach ($record in $records) {
        if (-not $AllowInUseRename) { CheckUnlocked $record.path }
        if ($record.existed -and (Hash $record.path) -ne $record.old_sha256) { throw "Target changed during installation: $($record.path)" }
        if (-not $record.existed -and (Test-Path -LiteralPath $record.path)) { throw "Target appeared during installation: $($record.path)" }
        $parent = Split-Path -Parent $record.path
        if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent | Out-Null }
        $attempted += $record
        if ($record.displaced) { MoveVerified $record.path $record.displaced $record.old_sha256 }
        Copy-Item -LiteralPath (Join-Path $stageDir $record.source) -Destination $record.path -Force
        if ((Hash $record.path) -ne $record.new_sha256) { throw "Installed checksum mismatch: $($record.path)" }
    }
    $manifest.state = 'installed'
    $manifest['completed_at'] = Get-Date -Format 'o'
    [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), $utf8)
} catch {
    $failure = $_.Exception.Message
    $rollbackFailures = @()
    for ($index=$attempted.Count-1; $index -ge 0; $index--) {
        $record = $attempted[$index]
        try {
            if ($record.displaced) {
                if (Test-Path -LiteralPath $record.displaced) {
                    CheckPath $record.path
                    if (Test-Path -LiteralPath $record.path) { Remove-Item -LiteralPath $record.path -Force }
                    MoveVerified $record.displaced $record.path $record.old_sha256
                } elseif (-not (Test-Path -LiteralPath $record.path -PathType Leaf) -or (Hash $record.path) -ne $record.old_sha256) { throw "Original image missing: $($record.path); displaced=$($record.displaced)" }
            }
            elseif ($record.existed) { Copy-Item -LiteralPath $record.backup -Destination $record.path -Force }
            elseif (Test-Path -LiteralPath $record.path) { Remove-Item -LiteralPath $record.path -Force }
            if ($record.existed -and (Hash $record.path) -ne $record.old_sha256) { throw 'Restored checksum mismatch' }
        } catch { $rollbackFailures += "$($record.path): $($_.Exception.Message); backup=$($record.backup)" }
    }
    $manifest.state = if ($rollbackFailures.Count) { 'rollback_failed' } else { 'rolled_back' }
    $manifest['failure'] = $failure
    $manifest['rollback_failures'] = $rollbackFailures
    [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), $utf8)
    throw "Local install failed: $failure; manifest=$manifestPath; rollback failures=$($rollbackFailures -join '; ')"
}
Write-Host "Installed version $($versions[0]); unsigned package checksums and 19-tool MCP handshakes verified."
Write-Host "Restore manifest: $manifestPath"
if ($AllowInUseRename) { Write-Host 'Running old images are retained in displaced/. Exit processes using the new files before restore-local.ps1.' }
# Return the durable manifest path for automation; no host config, PATH or .install metadata is edited.
$manifestPath
