param([string]$Workspace=(Get-Location).Path,[string]$Melissa='melissa',[string]$HomePath)
$ErrorActionPreference='Stop'
$projectRoot=(Resolve-Path -LiteralPath $Workspace).Path
$arguments=@()
if($HomePath){$arguments+=@('--home',$HomePath)}
$arguments+=@('--json','project','doctor','--workspace',$projectRoot)
& $Melissa @arguments
exit $LASTEXITCODE
