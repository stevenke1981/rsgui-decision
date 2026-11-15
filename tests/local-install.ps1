#requires -Version 5.1
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$PackageDir)
$ErrorActionPreference = 'Stop'
$PackageDir = [IO.Path]::GetFullPath($PackageDir)
$repo = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $repo 'scripts\install-local.ps1'
$restore = Join-Path $repo 'scripts\restore-local.ps1'
$utf8 = New-Object Text.UTF8Encoding($false)
function Assert($Condition,[string]$Message) { if (-not $Condition) { throw $Message } }
function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }
function WriteUtf8([string]$Path,[string]$Text) { [IO.File]::WriteAllText($Path,$Text,$utf8) }
function MustFail([scriptblock]$Action,[string]$Pattern) {
    $message = $null
    try { & $Action | Out-Null } catch { $message = $_.Exception.Message }
    Assert ($null -ne $message -and $message -match $Pattern) "Expected failure matching '$Pattern', got '$message'"
}
function FixturePackage([string]$Path,[switch]$MissingRuntime) {
    New-Item -ItemType Directory -Path $Path | Out-Null
    $entries = @()
    foreach ($name in @('autogui-control.exe','rsgui-decision.exe','autogui_runtime.dll')) {
        if ($MissingRuntime -and $name -eq 'autogui_runtime.dll') { continue }
        $target = Join-Path $Path $name
        Copy-Item -LiteralPath (Join-Path $PackageDir $name) -Destination $target
        $entries += @{path=$name;sha256=(Hash $target);bytes=(Get-Item -LiteralPath $target).Length}
    }
    $payloadPath = Join-Path $Path 'payload.json'
    WriteUtf8 $payloadPath (@{version=1;files=$entries} | ConvertTo-Json -Depth 5)
    $sums = @($entries | ForEach-Object { "$($_.sha256)  $($_.path)" })
    $sums += "$(Hash $payloadPath)  payload.json"
    [IO.File]::WriteAllLines((Join-Path $Path 'SHA256SUMS.txt'),$sums,$utf8)
}

$fixture = Join-Path $env:TEMP ('rsgui-local-install-' + [guid]::NewGuid().ToString('N'))
# Spaces, Chinese text and a nested path exercise paths without shell interpolation.
$install = Join-Path $fixture 'AutoGui 舊安裝\with spaces\program'
$bin = Join-Path $fixture 'profile 中文\local bin'
New-Item -ItemType Directory -Path (Join-Path $install '.install') | Out-Null
New-Item -ItemType Directory -Path $bin | Out-Null
$marker = Join-Path $install '.install\manifest.json'
$outside = Join-Path $fixture 'outside-target.txt'
$unrelated = Join-Path $bin 'unrelated.exe'
WriteUtf8 $marker '{"original_metadata":"preserve exactly"}'
WriteUtf8 $outside 'outside target must remain intact'
WriteUtf8 $unrelated 'unrelated bin content'
$originals = @{}
foreach ($path in @((Join-Path $install 'autogui-control.exe'),(Join-Path $install 'autogui_runtime.dll'),(Join-Path $bin 'rsgui-decision.exe'))) {
    WriteUtf8 $path ('original fixture: ' + (Split-Path -Leaf $path))
    $originals[$path] = Hash $path
}
$absentBefore = Join-Path $bin 'autogui_runtime.dll'
$markerHash = Hash $marker
$outsideHash = Hash $outside
$unrelatedHash = Hash $unrelated
try {
    $manifest1 = & $installer -PackageDir $PackageDir -InstallDir $install -BinDir $bin
    Assert (Test-Path -LiteralPath $manifest1 -PathType Leaf) 'First installation did not return a manifest'
    $data1 = [IO.File]::ReadAllText($manifest1,[Text.Encoding]::UTF8) | ConvertFrom-Json
    Assert ($data1.state -eq 'installed' -and $data1.files.Count -eq 4) 'Incomplete install manifest'
    foreach ($record in $data1.files) {
        Assert ((Hash $record.path) -eq (Hash (Join-Path $PackageDir $record.source))) "Wrong installed bytes: $($record.path)"
        if ($record.existed) { Assert ((Hash $record.backup) -eq $originals[$record.path]) "Wrong original backup: $($record.backup)" }
    }
    Assert ((Hash $marker) -eq $markerHash -and (Hash $outside) -eq $outsideHash -and (Hash $unrelated) -eq $unrelatedHash) 'Installation changed unrelated files'

    $manifest2 = & $installer -PackageDir $PackageDir -InstallDir $install -BinDir $bin
    Assert ($manifest1 -ne $manifest2) 'Reinstallation overwrote its previous recovery manifest'
    $data2 = [IO.File]::ReadAllText($manifest2,[Text.Encoding]::UTF8) | ConvertFrom-Json
    Assert (@($data2.files | Where-Object {-not $_.existed}).Count -eq 0) 'Reinstallation did not preserve all prior files'
    $changed = $data2.files[0]
    WriteUtf8 $changed.path 'user edited installed file'
    $changedHash = Hash $changed.path
    MustFail { & $restore -Manifest $manifest2 } 'Installed file changed'
    Assert ((Hash $changed.path) -eq $changedHash) 'Restore overwrote user changes'
    foreach ($record in @($data2.files | Select-Object -Skip 1)) { Assert ((Hash $record.path) -eq $record.new_sha256) 'Rejected restore partially changed other files' }
    Copy-Item -LiteralPath (Join-Path $PackageDir $changed.source) -Destination $changed.path -Force
    & $restore -Manifest $manifest2
    foreach ($record in $data1.files) { Assert ((Hash $record.path) -eq $record.new_sha256) 'Second restore failed to reinstate first installation' }

    $manifestText = [IO.File]::ReadAllText($manifest1,[Text.Encoding]::UTF8)
    $invalid = $manifestText | ConvertFrom-Json
    $invalid.files[0].path = $outside
    WriteUtf8 $manifest1 ($invalid | ConvertTo-Json -Depth 8)
    MustFail { & $restore -Manifest $manifest1 } 'Invalid restore target'
    Assert ((Hash $outside) -eq $outsideHash) 'Invalid manifest changed an outside path'
    WriteUtf8 $manifest1 $manifestText

    $oldRecord = @($data1.files | Where-Object {$_.existed})[0]
    $backupBytes = [IO.File]::ReadAllBytes($oldRecord.backup)
    WriteUtf8 $oldRecord.backup 'tampered backup'
    MustFail { & $restore -Manifest $manifest1 } 'Backup checksum mismatch'
    foreach ($record in $data1.files) { Assert ((Hash $record.path) -eq $record.new_sha256) 'Rejected backup partially restored files' }
    [IO.File]::WriteAllBytes($oldRecord.backup,$backupBytes)
    & $restore -Manifest $manifest1
    foreach ($path in $originals.Keys) { Assert ((Hash $path) -eq $originals[$path]) "Original bytes not restored: $path" }
    Assert (-not (Test-Path -LiteralPath $absentBefore)) 'Restore retained a file absent before install'
    Assert ((Hash $marker) -eq $markerHash -and (Hash $outside) -eq $outsideHash -and (Hash $unrelated) -eq $unrelatedHash) 'Restore changed unrelated files'

    # Inject a destination write failure after the program pair was replaced.
    # The actual package probes/copies run; only this one final write is denied.
    & {
        function Copy-Item {
            [CmdletBinding()]
            param([string]$LiteralPath,[string]$Destination,[switch]$Force)
            if ($Destination -eq $absentBefore) { throw 'Injected destination write failure' }
            Microsoft.PowerShell.Management\Copy-Item @PSBoundParameters
        }
        MustFail { & $installer -PackageDir $PackageDir -InstallDir $install -BinDir $bin } 'Injected destination write failure'
    }
    foreach ($path in $originals.Keys) { Assert ((Hash $path) -eq $originals[$path]) "Failed install did not roll back: $path" }
    Assert (-not (Test-Path -LiteralPath $absentBefore)) 'Failed install retained a newly created target'
    $transactions = @(Get-ChildItem -LiteralPath (Join-Path $install '.local-replacements') -Directory | ForEach-Object {
        [IO.File]::ReadAllText((Join-Path $_.FullName 'manifest.json'),[Text.Encoding]::UTF8) | ConvertFrom-Json
    })
    Assert (@($transactions | Where-Object {$_.state -eq 'rolled_back'}).Count -eq 1) 'Failed install did not record verified rollback'

    $lockedInstall = Join-Path $fixture 'locked install'
    New-Item -ItemType Directory -Path $lockedInstall | Out-Null
    $lockedPath = Join-Path $lockedInstall 'autogui-control.exe'
    WriteUtf8 $lockedPath 'locked original'
    $lockedHash = Hash $lockedPath
    $lock = [IO.File]::Open($lockedPath,[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    try { MustFail { & $installer -PackageDir $PackageDir -InstallDir $lockedInstall -BinDir (Join-Path $fixture 'locked bin') } 'autogui-control.exe' }
    finally { $lock.Dispose() }
    Assert ((Hash $lockedPath) -eq $lockedHash -and -not (Test-Path -LiteralPath (Join-Path $lockedInstall '.local-replacements'))) 'Locked target was modified or backed up after failure'

    $badPackage = Join-Path $fixture 'bad checksum package'
    FixturePackage $badPackage
    WriteUtf8 (Join-Path $badPackage 'autogui-control.exe') 'tampered binary'
    $rejectedInstall = Join-Path $fixture 'rejected install'
    MustFail { & $installer -PackageDir $badPackage -InstallDir $rejectedInstall -BinDir (Join-Path $fixture 'rejected bin') } 'Package checksum/length mismatch'
    Assert (-not (Test-Path -LiteralPath $rejectedInstall)) 'Invalid package created target directories'
    $incomplete = Join-Path $fixture 'incomplete package'
    FixturePackage $incomplete -MissingRuntime
    MustFail { & $installer -PackageDir $incomplete -InstallDir $rejectedInstall -BinDir (Join-Path $fixture 'rejected bin') } 'Missing mandatory package file'
    Assert (-not (Test-Path -LiteralPath $rejectedInstall)) 'Incomplete package created target directories'
    Write-Host 'PASS local replacement: staged version/MCP, hashes, unique reinstall backups, original metadata, tamper refusal, verified restore/failed-write rollback, absent-file removal, lock/package rejection.'
} finally { Write-Host "Local installer test evidence: $fixture" }
