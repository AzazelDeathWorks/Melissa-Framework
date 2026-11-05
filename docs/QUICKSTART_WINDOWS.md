# Windows x64 preview

The preview uses a Windows x64 CLI and Docker Desktop with the Linux engine for isolated build and test jobs. An offline task preparation example also runs without Docker or model credentials. These are separate checks.

## Verify and unpack

Download the Windows ZIP and its SHA-256 list from the same release. Run `Get-FileHash -Algorithm SHA256 <zip>` and compare the value with `SHA256SUMS.txt`. Extract the ZIP to a new directory. Its manifest identifies the source commit, executable hash and required SDK worker image.

In the extracted directory, run:

```powershell
.\melissa.exe --version
.\melissa.exe doctor
.\quickstart.ps1 -OutputRoot C:\MelissaDemo
```

Choose a new output directory. The example creates a scratch workspace, a separate faulty control and an inspectable recipe. It checks that the inspection task recorded zero model calls. It also prints the paths for the next step. Use `-RequireWorker` if you want the command to require a ready Docker engine and the exact SDK worker.

If Doctor reports the Linux engine is unavailable, start Docker Desktop in Linux container mode. If it reports a missing SDK worker, use a verified archive from the matching release. Compare the archive hash, run `docker image load --input <worker-archive>`, then rerun `doctor`. The full image ID must match the release manifest. A tag alone does not verify an image. Builds from the repositoryâ€™s toolchain recipes can require a reviewed immutable image registration.

## Run the example with your own model connection

Copy `settings.template.json` outside the example workspace. Fill in your endpoint, exact model identifier, declared capabilities, context size, output capacity and verified rate card. Keep the API key in a user-scoped environment variable or private file. The template values are placeholders. Use a model connection you have tested for streaming and tools.

Use the paths returned by the example. Review the file scope and acceptance cases in the prepared recipe. These expected answers are synthetic demo material. Keep real acceptance recipes outside the source workspace.

```powershell
$homePath = 'C:\MelissaDemo\melissa-home'
$source = 'C:\MelissaDemo\workspace'
$recipe = 'C:\MelissaDemo\prepared-recipe.json'
.\melissa.exe --home $homePath config import C:\MelissaDemo\provider-settings.json
$task = .\melissa.exe --home $homePath --json task new --workspace $source --goal 'Reverse the JSON array in main.py' --recipe $recipe | ConvertFrom-Json
.\melissa.exe --home $homePath task plan $task.task_id
```

The task plan shows the approval request ID and digest. Approve only after reviewing the files, cost and duration. Starting the task can incur provider charges.

```powershell
.\melissa.exe --home $homePath task approve $task.task_id --request <request-id> --digest <digest>
.\melissa.exe --home $homePath task run $task.task_id
.\melissa.exe --home $homePath task progress $task.task_id
.\melissa.exe --home $homePath task delivery $task.task_id --out C:\MelissaDemo\delivery
```

Delivery is available after independent protected verification. For support, share the CLI version, worker readiness and a redacted error message. Keep the task store, recipe, private settings and source content private.
