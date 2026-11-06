param([Parameter(Mandatory)][string]$OutputDirectory,[switch]$Build)
$ErrorActionPreference='Stop'
$manifest=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'tools.json') -Raw | ConvertFrom-Json
$destination=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $destination){throw 'Output directory must be new; existing evidence is preserved.'}
New-Item -ItemType Directory -Path $destination | Out-Null
foreach($asset in $manifest.assets){
  $archive=Join-Path $destination $asset.file
  Invoke-WebRequest -Uri $asset.url -OutFile $archive
  if((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $asset.sha256){throw ('Checksum mismatch: '+$asset.name)}
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'Dockerfile') -Destination (Join-Path $destination 'Dockerfile')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'tools.json') -Destination (Join-Path $destination 'inputs.json')
if($Build){
  $baseId=docker --host npipe:////./pipe/dockerDesktopLinuxEngine image inspect melissa-code06-complete:20261020 --format '{{.Id}}'
  if($LASTEXITCODE -ne 0 -or $baseId -ne $manifest.base_image){throw 'Local base tag does not match the reviewed immutable image ID.'}
  docker --host npipe:////./pipe/dockerDesktopLinuxEngine build --pull=false --iidfile (Join-Path $destination 'image.id') -t melissa-verification:0.1 $destination
  if($LASTEXITCODE -ne 0){throw 'Verification image build failed; inputs and logs are preserved.'}
}
