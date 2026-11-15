#requires -Version 5.1
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$Manifest)
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
$Manifest = FullPath $Manifest
CheckPath $Manifest
$data = [IO.File]::ReadAllText($Manifest, [Text.Encoding]::UTF8) | ConvertFrom-Json
if ($data.schema -ne 1 -or $data.state -ne 'installed' -or $data.transaction_id -notmatch '^\d{8}_\d{6}_[a-f0-9]{32}$') { throw 'Not a completed local-install manifest' }
foreach ($root in @($data.install_dir,$data.bin_dir)) {
    if (-not [IO.Path]::IsPathRooted($root) -or (FullPath $root) -ne $root) { throw 'Manifest roots must be canonical absolute paths' }
    CheckPath $root
}
$transactionRoot = Join-Path $data.install_dir ('.local-replacements\' + $data.transaction_id)
if ($Manifest -ne (Join-Path $transactionRoot 'manifest.json')) { throw 'Manifest location does not match its transaction' }
$backupDir = Join-Path $transactionRoot 'backup'
$stageDir = Join-Path $transactionRoot 'stage'
$allowed = @{}
$allowed[(Join-Path $data.install_dir 'autogui-control.exe')] = 'autogui-control.exe'
$allowed[(Join-Path $data.install_dir 'autogui_runtime.dll')] = 'autogui_runtime.dll'
$allowed[(Join-Path $data.bin_dir 'rsgui-decision.exe')] = 'rsgui-decision.exe'
$allowed[(Join-Path $data.bin_dir 'autogui_runtime.dll')] = 'autogui_runtime.dll'
$records = @($data.files)
if ($records.Count -ne $allowed.Count) { throw 'Manifest target count mismatch' }
$seen = @{}
for ($index=0; $index -lt $records.Count; $index++) {
    $record = $records[$index]
    if (-not [IO.Path]::IsPathRooted($record.path) -or (FullPath $record.path) -ne $record.path -or -not $allowed.ContainsKey($record.path) -or $seen.ContainsKey($record.path) -or $record.source -ne $allowed[$record.path] -or $record.existed -isnot [bool] -or $record.new_sha256 -notmatch '^[A-Fa-f0-9]{64}$') { throw "Invalid restore target: $($record.path)" }
    $seen[$record.path] = $true
    CheckPath $record.path
    if (-not (Test-Path -LiteralPath $record.path -PathType Leaf) -or (Hash $record.path) -ne $record.new_sha256) { throw "Installed file changed; refusing restore: $($record.path)" }
    $staged = Join-Path $stageDir $record.source
    CheckPath $staged
    if (-not (Test-Path -LiteralPath $staged -PathType Leaf) -or (Hash $staged) -ne $record.new_sha256) { throw "Staged recovery checksum mismatch: $staged" }
    if ($record.existed) {
        $expectedBackup = Join-Path $backupDir ('{0:D2}.bin' -f $index)
        if ($record.backup -ne $expectedBackup -or $record.old_sha256 -notmatch '^[A-Fa-f0-9]{64}$') { throw "Invalid backup path/hash: $($record.path)" }
        CheckPath $record.backup
        if (-not (Test-Path -LiteralPath $record.backup -PathType Leaf) -or (Hash $record.backup) -ne $record.old_sha256) { throw "Backup checksum mismatch: $($record.backup)" }
    } elseif ($null -ne $record.backup -or $null -ne $record.old_sha256) { throw "Unexpected backup for a new file: $($record.path)" }
    try {
        $stream = [IO.File]::Open($record.path, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        $stream.Dispose()
    } catch { throw "Exit processes using the newly installed file before restoring: $($record.path). $($_.Exception.Message)" }
}
$attempted = @()
try {
    foreach ($record in $records) {
        if ((Hash $record.path) -ne $record.new_sha256) { throw "Installed file changed during restore: $($record.path)" }
        $attempted += $record
        if ($record.existed) {
            Copy-Item -LiteralPath $record.backup -Destination $record.path -Force
            if ((Hash $record.path) -ne $record.old_sha256) { throw "Restored checksum mismatch: $($record.path)" }
        } else { Remove-Item -LiteralPath $record.path -Force }
    }
    $data.state = 'restored'
    $data | Add-Member -NotePropertyName restored_at -NotePropertyValue (Get-Date -Format 'o')
    [IO.File]::WriteAllText($Manifest, ($data | ConvertTo-Json -Depth 8), $utf8)
} catch {
    $failure = $_.Exception.Message
    $rollbackFailures = @()
    foreach ($record in $attempted) {
        try {
            Copy-Item -LiteralPath (Join-Path $stageDir $record.source) -Destination $record.path -Force
            if ((Hash $record.path) -ne $record.new_sha256) { throw 'Recovery checksum mismatch' }
        } catch { $rollbackFailures += "$($record.path): $($_.Exception.Message)" }
    }
    throw "Restore failed: $failure; manifest=$Manifest; recovery failures=$($rollbackFailures -join '; ')"
}
Write-Host "Restored verified original files; removed only explicitly recorded new files. Manifest: $Manifest"
