[CmdletBinding()]
param(
    [string]$Melissa = (Join-Path $PSScriptRoot 'melissa.exe'),
    [Parameter(Mandatory)][string]$OutputRoot,
    [switch]$RequireWorker
)
$ErrorActionPreference = 'Stop'
$Melissa = (Resolve-Path -LiteralPath $Melissa).Path

function Invoke-MelissaJson {
    param([string[]]$Command)
    $output = & $Melissa --json @Command
    if ($LASTEXITCODE -ne 0) { throw "Melissa command failed ($LASTEXITCODE): $($Command[0])" }
    return ($output | Out-String | ConvertFrom-Json)
}

$doctor = Invoke-MelissaJson @('doctor')
if ($doctor.host -ne 'windows' -or $doctor.architecture -ne 'x86_64') {
    throw 'Use a Windows x64 Melissa executable.'
}
if ($doctor.credential_read -ne $false -or $doctor.model_probe -ne 'NOT_RUN') {
    throw 'The offline doctor unexpectedly probed the model.'
}
if ($RequireWorker -and $doctor.release_readiness.status -ne 'PASS') {
    throw $doctor.release_readiness.next_step
}
$root = (New-Item -ItemType Directory -Path $OutputRoot -ErrorAction Stop).FullName
$source = (New-Item -ItemType Directory -Path (Join-Path $root 'workspace')).FullName
$negative = (New-Item -ItemType Directory -Path (Join-Path $root 'negative')).FullName
$homePath = Join-Path $root 'melissa-home'
$demo = Join-Path $root 'example-recipe.json'
$prepared = Join-Path $root 'prepared-recipe.json'
$goal = 'Fix main.py so it reads a JSON array from argv[1] and returns the reversed array as JSON.'
Set-Content -LiteralPath (Join-Path $source 'main.py') -Encoding ascii -Value @(
    'import json, sys', 'print(json.dumps(json.loads(sys.argv[1])))'
)
Set-Content -LiteralPath (Join-Path $negative 'main.py') -Encoding ascii -Value 'print("null")'
$recipe = [ordered]@{
    schema_version = 'melissa.cli-recipe/v2'
    agents = @(@{profile = 'melissa.code.implementer'; goal = $goal; write = $true})
    writable_paths = @('main.py')
    protected_paths = @()
    negative_workspace = $negative
    max_cost_micro_usd = 2000000
    wall_time_ms = 600000
    language = 'python'
    entry = 'main.py'
    development_checks = @(
        @{id = 'public-1'; input = @(1, 2); expected = @(2, 1)},
        @{id = 'public-2'; input = @('a', 'b'); expected = @('b', 'a')}
    )
    checks = @(
        @{id = 'final-1'; input = @(9, 4, 1); expected = @(1, 4, 9)},
        @{id = 'final-2'; input = @(-2, 0, 3); expected = @(3, 0, -2)}
    )
}
[IO.File]::WriteAllText($demo, ($recipe | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
$null = Invoke-MelissaJson @('--home', $homePath, 'init')
$null = Invoke-MelissaJson @('--home', $homePath, 'task', 'prepare', '--workspace', $source, '--goal', $goal, '--out', $prepared, '--acceptance', $demo)
if (-not (Test-Path -LiteralPath $prepared -PathType Leaf)) { throw 'Task preparation did not create a recipe.' }
$task = Invoke-MelissaJson @('--home', $homePath, 'task', 'new', '--workspace', $source, '--goal', $goal)
$metrics = Invoke-MelissaJson @('--home', $homePath, 'task', 'metrics', $task.task_id)
if ($metrics.metrics.model_dispatch_attempts -ne 0 -or $metrics.metrics.verified_acceptance -eq $true) {
    throw 'An offline inspection task has unexpected execution or acceptance evidence.'
}
[ordered]@{
    status = 'PASS'
    scope = 'offline CLI initialization, repository inspection and recipe preparation'
    worker_readiness = $doctor.release_readiness
    model_dispatches = $metrics.metrics.model_dispatch_attempts
    verified_acceptance = $metrics.metrics.verified_acceptance
    task_id = $task.task_id
    workspace = $source
    home = $homePath
    example_recipe = $demo
    prepared_recipe = $prepared
} | ConvertTo-Json -Depth 5
