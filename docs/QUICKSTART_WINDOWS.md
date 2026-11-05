# Windows x64 preview

The preview uses a Windows x64 CLI and Docker Desktop with the Linux engine for isolated build and test jobs. An offline task preparation example also runs without Docker or model credentials. These are separate checks.

## Install with one command

Put `install.ps1`, the Windows ZIP and `SHA256SUMS.txt` from the same build in one directory. In PowerShell, run:

```powershell
.\install.ps1
```

The installer checks the ZIP and executable hashes, installs into `%LOCALAPPDATA%\Programs\Melissa`, and registers the command in the current and future PowerShell sessions for your account. During an update it keeps the previous executable under `previous` so you can reopen tasks tied to its exact binary.

For a published version, download the installer from the matching GitHub Release and run `.\install.ps1 -Version 0.3.0-alpha.3` to download and verify its package.

Open PowerShell in your project directory and run `melissa`. Type your task to prepare a plan locally. Use `/help` for the session commands. You can also use `melissa session --workspace C:\Projects\Example` to select a project explicitly.

The interactive session shows a project panel and compact task cards. Use the left and right arrows to edit, the up and down arrows for this session's history, and Tab to complete an unambiguous slash command. Esc clears the input. Ctrl+C clears a nonempty input or exits from an empty prompt. History stays in memory. Terminal resizing keeps the input cursor visible. Set `NO_COLOR` to disable colors. Redirected input and output retain the plain session format. Use `/details` to see the complete recorded task scope.

For a model-backed task, use `/config PATH` for private model settings and `/acceptance PATH` for an operator-authored recipe outside the project. Then type the task, review the draft, and use `/create` to see the exact scope and budget. `/approve` records approval and `/run` starts model-backed execution. Use `/status` and `/diff` to inspect the result. Use `/open TASK_ID` in the same project to reopen a saved task.

## Verify and unpack for the offline example

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
