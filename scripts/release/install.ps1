[CmdletBinding()]
param(
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+$')][string]$Version,
    [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA 'Programs\Melissa'),
    [switch]$NoPath
)
$ErrorActionPreference = 'Stop'
$hostArchitecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($env:OS -ne 'Windows_NT' -or $hostArchitecture -ne 'AMD64') { throw 'Melissa requires Windows x64.' }

$downloads = @()
$staged = $null
$lock = $null
$previous = $null
try {
    if ($Version) {
        $archiveName = "melissa-$Version-windows-x64.zip"
        $release = "https://github.com/AzazelDeathWorks/Melissa-Framework/releases/download/v$Version"
        $archivePath = [IO.Path]::GetTempFileName()
        $downloads += $archivePath
        $checksums = [IO.Path]::GetTempFileName()
        $downloads += $checksums
        Invoke-WebRequest -UseBasicParsing -Uri "$release/SHA256SUMS.txt" -OutFile $checksums
        Invoke-WebRequest -UseBasicParsing -Uri "$release/$archiveName" -OutFile $archivePath
    } else {
        if (-not $PSScriptRoot) { throw 'Provide -Version when running the installer without a local package.' }
        $packages = @(Get-ChildItem -LiteralPath $PSScriptRoot -Filter 'melissa-*-windows-x64.zip' -File)
        if ($packages.Count -ne 1) { throw 'Place one Melissa ZIP and its SHA256SUMS.txt beside install.ps1, or provide -Version.' }
        $archivePath = $packages[0].FullName
        $archiveName = $packages[0].Name
        $checksums = Join-Path $PSScriptRoot 'SHA256SUMS.txt'
    }
    $packageName = [regex]::Match($archiveName, '^melissa-([0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+)-windows-x64\.zip$')
    if (-not $packageName.Success) { throw 'Unexpected Windows package name.' }
    $hash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $expected = "$hash  $archiveName"
    if ((Get-Content -LiteralPath $checksums -Raw).Trim() -cne $expected) { throw 'Melissa ZIP checksum mismatch.' }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $manifests = @($zip.Entries | Where-Object FullName -CEQ 'release-manifest.json')
        $executables = @($zip.Entries | Where-Object FullName -CEQ 'melissa.exe')
        if ($manifests.Count -ne 1 -or $executables.Count -ne 1) { throw 'Expected one manifest and one Melissa executable.' }
        $reader = [IO.StreamReader]::new($manifests[0].Open())
        try { $manifest = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
        if ($manifest.schema_version -cne 'melissa.release/v1' -or $manifest.platform -cne 'windows-x64' -or
            $manifest.version -cne $packageName.Groups[1].Value -or $manifest.source_commit -cnotmatch '^[a-f0-9]{40}$' -or
            $manifest.executable_sha256 -cnotmatch '^[a-f0-9]{64}$') { throw 'Unexpected Melissa release manifest.' }

        $root = [IO.Path]::GetFullPath($InstallRoot)
        if ($root.Contains(';')) { throw 'The installation path cannot contain a semicolon.' }
        [void][IO.Directory]::CreateDirectory($root)
        $lock = [IO.File]::Open((Join-Path $root '.install.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $staged = Join-Path $root ('.melissa-' + [guid]::NewGuid().ToString('N') + '.tmp')
        $target = Join-Path $root 'melissa.exe'
        $sink = [IO.File]::Open($staged, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
            $source = $executables[0].Open()
            try { $source.CopyTo($sink) } finally { $source.Dispose() }
        } finally { $sink.Dispose() }
        if ((Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash.ToLowerInvariant() -cne $manifest.executable_sha256) {
            throw 'Melissa executable checksum mismatch.'
        }
        if ([IO.File]::Exists($target)) {
            $previousHash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($previousHash -cne $manifest.executable_sha256) {
                $previousRoot = Join-Path $root 'previous'
                [void][IO.Directory]::CreateDirectory($previousRoot)
                $previous = Join-Path $previousRoot "melissa-$previousHash.exe"
                [IO.File]::Replace($staged, $target, $previous)
            }
        } else {
            [IO.File]::Move($staged, $target)
        }
    } finally { $zip.Dispose() }

    if (-not $NoPath) {
        $normalizedRoot = $root.TrimEnd('\', '/')
        $samePath = {
            [Environment]::ExpandEnvironmentVariables(([string]$_).Trim().Trim('"')).TrimEnd('\', '/') -eq $normalizedRoot
        }
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
        try {
            $userPath = $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            if (-not ($userPath -split ';' | Where-Object $samePath)) {
                $newPath = if ($userPath) { "$root;$userPath" } else { $root }
                $kind = if ($null -ne $userPath) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
                if (-not ('Melissa.Install.WindowsEnvironment' -as [type])) {
                    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace Melissa.Install {
    public static class WindowsEnvironment {
        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, IntPtr wParam,
            string lParam, uint flags, uint timeout, out IntPtr result);
        public static void Notify() {
            IntPtr result;
            SendMessageTimeout(new IntPtr(0xffff), 0x001a, IntPtr.Zero, "Environment", 2, 1000, out result);
        }
    }
}
'@
                }
                $key.SetValue('Path', $newPath, $kind)
                [Melissa.Install.WindowsEnvironment]::Notify()
            }
        } finally { $key.Dispose() }
        $otherPaths = @($env:Path -split ';' | Where-Object { -not (& $samePath) })
        $env:Path = (@($root) + $otherPaths) -join ';'
    }
    [pscustomobject]@{ status = 'installed'; version = $manifest.version; executable = $target; path_registered = -not $NoPath; previous_executable = $previous }
} finally {
    if ($lock) { $lock.Dispose() }
    foreach ($file in @($staged) + $downloads) {
        if ($file -and [IO.File]::Exists($file)) { [IO.File]::Delete($file) }
    }
}
