param([Parameter(Mandatory=$true)][string]$TaskId,[Parameter(Mandatory=$true)][string]$Source,[Parameter(Mandatory=$true)][string]$Query,[string]$Melissa='melissa',[string]$HomePath)
$ErrorActionPreference='Stop'
$arguments=@()
if($HomePath){$arguments+=@('--home',$HomePath)}
$arguments+=@('--json','extension','resolve','--task',$TaskId,'--source',$Source,'--query',$Query)
& $Melissa @arguments
exit $LASTEXITCODE
