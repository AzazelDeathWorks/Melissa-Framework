param([Parameter(Mandatory)][string]$TaskId,[Parameter(Mandatory)][string]$Name,[string]$Melissa='melissa',[string]$MelissaHome)
$ErrorActionPreference='Stop'
$arguments=@('--json')
if($MelissaHome){$arguments=@('--home',$MelissaHome)+$arguments}
$request=@{operation='migration';name=$Name} | ConvertTo-Json -Compress
& $Melissa @arguments services run $TaskId --request-json $request
if($LASTEXITCODE -ne 0){throw "Migration rehearsal did not pass ($LASTEXITCODE); preserve schema/data and rollback evidence."}
