param([Parameter(Mandatory)][string]$TaskId,[string[]]$Checks=@(),[string]$Baseline,[string]$Melissa='melissa',[string]$MelissaHome)
$ErrorActionPreference='Stop'
$request=@{names=@($Checks)}
if($Baseline){$request.baseline=$Baseline}
$arguments=@('--json')
if($MelissaHome){$arguments=@('--home',$MelissaHome)+$arguments}
& $Melissa @arguments verification verify $TaskId --request-json ($request | ConvertTo-Json -Compress)
if($LASTEXITCODE -ne 0){throw "verify-change failed ($LASTEXITCODE); preserve the receipt and inspect the task"}
