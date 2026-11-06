param([Parameter(Mandatory)][string]$TaskId,[string[]]$Checks=@(),[string]$Melissa='melissa',[string]$MelissaHome)
$ErrorActionPreference='Stop'
$arguments=@('--json')
if($MelissaHome){$arguments=@('--home',$MelissaHome)+$arguments}
& $Melissa @arguments verification reproduce $TaskId --request-json (@{names=@($Checks)} | ConvertTo-Json -Compress)
if($LASTEXITCODE -ne 0){throw "reproduce-failure failed ($LASTEXITCODE); preserve the original failure"}
