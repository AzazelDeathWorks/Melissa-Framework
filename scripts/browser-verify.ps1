param([Parameter(Mandatory)][string]$Task,[Parameter(Mandatory)][string]$Check,[string]$Melissa='melissa',[string]$HomeDirectory)
$ErrorActionPreference='Stop'
$options=@();if($HomeDirectory){$options+=@('--home',$HomeDirectory)}
$request=@{operation='browser';command=@{command='verify';name=$Check}} | ConvertTo-Json -Compress -Depth 4
& $Melissa @options --json services run $Task --request-json $request
exit $LASTEXITCODE
