[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BinaryPath,
    [Parameter(Mandatory)][string]$OutputRoot,
    [switch]$LocalPreview
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$binary = (Resolve-Path -LiteralPath $BinaryPath).Path
$manifestText = Get-Content -LiteralPath (Join-Path $repo 'crates\cli\Cargo.toml') -Raw
$versionMatch = [regex]::Match($manifestText, '(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+)"')
if (-not $versionMatch.Success) { throw 'Expected an alpha version in the CLI manifest.' }
$version = $versionMatch.Groups[1].Value
$versionOutput = & $binary --version
if ($LASTEXITCODE -ne 0 -or $versionOutput -ne "melissa $version") { throw 'Binary version differs from the CLI manifest.' }
$doctorRaw = & $binary --json doctor
if ($LASTEXITCODE -ne 0) { throw 'The release binary failed its doctor probe.' }
$doctor = $doctorRaw | Out-String | ConvertFrom-Json
if ($doctor.host -ne 'windows' -or $doctor.architecture -ne 'x86_64' -or $doctor.credential_read -ne $false) {
    throw 'The release binary must be Windows x64 with a credential-free doctor probe.'
}
$revision = & git -C $repo rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $revision -notmatch '^[0-9a-f]{40}$') { throw 'The release source revision could not be determined.' }
if ($doctor.source_commit -ne $revision) { throw 'Build the binary with MELISSA_BUILD_COMMIT set to the exact source revision.' }
$changes = & git -C $repo status --porcelain --untracked-files=normal
if ($LASTEXITCODE -ne 0) { throw 'Source status could not be determined.' }
if ($changes -and -not $LocalPreview) { throw 'Build the release from a clean, committed checkout. Use -LocalPreview only for an explicitly marked local candidate.' }
$sourceFiles = & git -C $repo ls-files --cached --others --exclude-standard
if ($LASTEXITCODE -ne 0) { throw 'Source inventory could not be determined.' }
$sourceInventory = @($sourceFiles | Sort-Object -Unique | ForEach-Object {
    $path = Join-Path $repo $_
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        "$((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant())  $_"
    }
})
$sha = [Security.Cryptography.SHA256]::Create()
try { $sourceHash = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes(($sourceInventory -join "`n"))))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
$target = (New-Item -ItemType Directory -Path $OutputRoot -ErrorAction Stop).FullName
$archiveName = "melissa-$version-windows-x64.zip"
$archivePath = Join-Path $target $archiveName
$binaryHash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
$payload = [ordered]@{
    schema_version = 'melissa.release/v1'
    version = $version
    platform = 'windows-x64'
    source_commit = $revision
    source_tree_sha256 = $sourceHash
    source_dirty = [bool]$changes
    local_preview = [bool]$LocalPreview
    executable_sha256 = $binaryHash
    sdk_worker_image_id = $doctor.isolated_SDK_image_id
} | ConvertTo-Json
$files = [ordered]@{
    'LICENSE' = Join-Path $repo 'LICENSE'
    'QUICKSTART.md' = Join-Path $repo 'docs\QUICKSTART_WINDOWS.md'
    'CLI_TR.md' = Join-Path $repo 'docs\CLI_TR.md'
    'PROJECT_RUNTIME_TR.md' = Join-Path $repo 'docs\PROJECT_RUNTIME_TR.md'
    'SECURITY.md' = Join-Path $repo 'SECURITY.md'
    'melissa.exe' = $binary
}
$notices = Join-Path $target 'license-staging'
& python (Join-Path $PSScriptRoot 'collect-rust-licenses.py') --repo $repo --out $notices
if ($LASTEXITCODE -ne 0) { throw 'Exact dependency notice collection failed.' }
foreach ($file in (Get-ChildItem -LiteralPath $notices -File -Recurse | Sort-Object FullName)) {
    $relative = $file.FullName.Substring($notices.Length + 1).Replace('\', '/')
    $files["licenses/$relative"] = $file.FullName
}

Add-Type -AssemblyName System.IO.Compression
$stream = [System.IO.File]::Open($archivePath, [System.IO.FileMode]::CreateNew)
try {
    $zip = [System.IO.Compression.ZipArchive]::new($stream, [System.IO.Compression.ZipArchiveMode]::Create, $true)
    try {
        foreach ($item in $files.GetEnumerator()) {
            $entry = $zip.CreateEntry($item.Key, [System.IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = [DateTimeOffset]'1980-01-01T00:00:00Z'
            $source = [System.IO.File]::OpenRead($item.Value)
            try {
                $sink = $entry.Open()
                try { $source.CopyTo($sink) } finally { $sink.Dispose() }
            } finally { $source.Dispose() }
        }
        $entry = $zip.CreateEntry('release-manifest.json')
        $entry.LastWriteTime = [DateTimeOffset]'1980-01-01T00:00:00Z'
        $writer = [System.IO.StreamWriter]::new($entry.Open(), [System.Text.UTF8Encoding]::new($false))
        try { $writer.WriteLine($payload) } finally { $writer.Dispose() }
    } finally { $zip.Dispose() }
} finally { $stream.Dispose() }

$hash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath (Join-Path $target 'SHA256SUMS.txt') -Encoding ascii -Value "$hash  $archiveName"
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install.ps1') -Destination $target
[pscustomobject]@{archive = $archivePath; sha256 = $hash; version = $version; source_commit = $revision}
