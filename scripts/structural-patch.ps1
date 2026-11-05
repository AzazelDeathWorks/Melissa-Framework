param([Parameter(Mandatory)][string]$Workspace,[Parameter(Mandatory)][string]$Query,[string]$Melissa='melissa',[string]$MelissaHome)
$ErrorActionPreference='Stop'
$arguments=@('--json')
if($MelissaHome){$arguments=@('--home',$MelissaHome)+$arguments}
& $Melissa @arguments repository structural-patch --workspace $Workspace --query $Query
if($LASTEXITCODE -ne 0){throw "structural-patch failed ($LASTEXITCODE)"}
