param([Parameter(Mandatory=$true)][string]$TaskId,[string]$Melissa='melissa',[string]$HomePath)
$ErrorActionPreference='Stop'
$arguments=@()
if($HomePath){$arguments+=@('--home',$HomePath)}
$arguments+=@('--json','project','sync',$TaskId)
# The approved task owns its image, tools, limits and lockfile-bound procedures.
# The CLI never installs into the host/global environment or enables networking.
& $Melissa @arguments
exit $LASTEXITCODE
