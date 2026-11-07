param([Parameter(Mandatory)][string]$OutputDirectory,[switch]$Build)
$ErrorActionPreference='Stop'
$destination=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $destination){throw 'Use a new directory; existing SDK inputs and evidence stay preserved.'}
New-Item -ItemType Directory -Path $destination | Out-Null
foreach($name in @('Dockerfile','package.json','package-lock.json')){Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $destination $name)}
if($Build){
 $base='sha256:d853da333281e062219bfdffe0fce754dbf01742153032e2e3ae3c71a71aeb39'
 $observed=& docker --host npipe:////./pipe/dockerDesktopLinuxEngine image inspect $base --format '{{.Id}}'
 if($LASTEXITCODE -ne 0 -or $observed -ne $base){throw 'The reviewed immutable service SDK base must already exist locally.'}
 $alias='melissa-frontend-base:d853da333281'
 $existing=& docker --host npipe:////./pipe/dockerDesktopLinuxEngine image inspect $alias --format '{{.Id}}' 2>$null
 if($LASTEXITCODE -ne 0){& docker --host npipe:////./pipe/dockerDesktopLinuxEngine image tag $base $alias;if($LASTEXITCODE -ne 0){throw 'Could not add the verified base alias.'}}
 elseif($existing -ne $base){throw 'Existing alias differs and was not replaced.'}
 & docker --host npipe:////./pipe/dockerDesktopLinuxEngine build --pull=false --iidfile (Join-Path $destination 'image.id') $destination
 if($LASTEXITCODE -ne 0){throw 'Frontend SDK build failed; inspect retained output.'}
}
