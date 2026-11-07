param([Parameter(Mandatory)][string]$OutputDirectory,[switch]$Build)
$ErrorActionPreference='Stop'
$manifest=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'tools.json') -Raw | ConvertFrom-Json
$destination=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $destination){throw 'Use a new output directory; existing inputs and evidence are preserved.'}
New-Item -ItemType Directory -Path (Join-Path $destination 'wheels') | Out-Null
foreach($asset in $manifest.assets){
 $path=Join-Path (Join-Path $destination 'wheels') $asset.file
 Invoke-WebRequest -Uri $asset.url -OutFile $path
 if((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $asset.sha256){throw ('Wheel checksum mismatch: '+$asset.name)}
}
foreach($name in @('Dockerfile','requirements.lock','tools.json')){Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $destination $name)}
if($Build){
 $dockerPath=Join-Path $env:ProgramFiles 'Docker/Docker/resources/bin/docker.exe'
 $baseId=& $dockerPath --host npipe:////./pipe/dockerDesktopLinuxEngine image inspect $manifest.base_image --format '{{.Id}}'
 if($LASTEXITCODE -ne 0 -or $baseId -ne $manifest.base_image){throw 'The reviewed immutable SDK base must already exist locally.'}
 $alias='melissa-services-base:1b4ea48f329d'
 $existing=& $dockerPath --host npipe:////./pipe/dockerDesktopLinuxEngine image inspect $alias --format '{{.Id}}' 2>$null
 if($LASTEXITCODE -ne 0){& $dockerPath --host npipe:////./pipe/dockerDesktopLinuxEngine image tag $baseId $alias;if($LASTEXITCODE -ne 0){throw 'Could not create the verified local base alias.'}}
 elseif($existing -ne $baseId){throw 'Existing base alias points to another image; it was not replaced.'}
 & $dockerPath --host npipe:////./pipe/dockerDesktopLinuxEngine build --pull=false --iidfile (Join-Path $destination 'image.id') -t melissa-services:0.1 $destination
 if($LASTEXITCODE -ne 0){throw 'Service toolchain build failed; inputs and logs remain.'}
}
