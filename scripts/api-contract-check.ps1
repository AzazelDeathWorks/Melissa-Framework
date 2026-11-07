param([Parameter(Mandatory)][string]$TaskId,[Parameter(Mandatory)][string]$Name,[string]$Melissa='melissa',[string]$MelissaHome)
$ErrorActionPreference='Stop'
$arguments=@('--json')
if($MelissaHome){$arguments=@('--home',$MelissaHome)+$arguments}
$request=@{operation='contract';name=$Name} | ConvertTo-Json -Compress
& $Melissa @arguments services run $TaskId --request-json $request
if($LASTEXITCODE -ne 0){throw "API contract check did not pass ($LASTEXITCODE); failing cases and seed remain in task evidence."}
