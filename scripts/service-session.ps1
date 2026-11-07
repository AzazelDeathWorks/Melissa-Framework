param([Parameter(Mandatory)][string]$TaskId,[string]$Melissa='melissa',[string]$MelissaHome)
$ErrorActionPreference='Stop'
$arguments=@('--json')
if($MelissaHome){$arguments=@('--home',$MelissaHome)+$arguments}
& $Melissa @arguments services session $TaskId
if($LASTEXITCODE -ne 0){throw "Service session ended without clean success ($LASTEXITCODE); inspect retained task evidence."}
