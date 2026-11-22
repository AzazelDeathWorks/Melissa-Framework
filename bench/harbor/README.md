# Melissa / Harbor bridge

This is an **experimental benchmark adapter**, not the production CLI security
boundary. Gateway owns model generation and accounting. Harbor owns task
containers and the independent verifier; the bridge's bash/file tools do not
exercise all production repository/broker authorization checks. A final model
answer is not a pass. No low-memory, safety, or competitive-score claim follows
from this adapter alone.

File transfers require a POSIX host with `O_NOFOLLOW` and Linux task utilities
(`stat`, `mktemp`, `cat`, `mv -T`). Copied links are never followed on the
host, and symlink write destinations are rejected; use the bash
tool to inspect and explicitly select the intended container path. Reads are
limited to 16 MiB per transfer; larger files can be inspected with bounded bash
commands. Write payloads use private host transfer directories, not command-line
arguments or environment overrides; uploaded files are copied to agent-owned
staging before replacement. Live logs and child stores stay in private scratch.

## Reproduce

Tested with Harbor **0.26.0**, Python >=3.12 and `terminal-bench@2.0`. These are
not Terminal-Bench 4.0 results. Docker is required for the task environments;
image storage can exceed 50 GB. Do not prune other users' containers/images.

```sh
uv tool install --python 3.13 'harbor==0.26.0'
cargo build --locked --release -p melissa-bench-agent

# Set MELISSA_BENCH_API_KEY securely on the host. Never put it in task files,
# Harbor agent kwargs, shell command arguments, or committed configuration.
export MELISSA_BENCH_ENDPOINT=https://your-provider.example/v1/chat/completions
export MELISSA_BENCH_PROTOCOL=open_ai_chat_completions
PYTHONPATH=bench/harbor harbor run -d terminal-bench@2.0 \
  --agent-import-path melissa_agent:MelissaAgent -m your-exact-model-id \
  --plugin melissa_agent:MelissaDeadlinePlugin -n 2
```

The deadline plugin is for **single-step tasks**. It reads only host-side
`task.toml` metadata (never tests/solutions), applying Harbor's timeout override,
maximum and multiplier in the same order as Harbor. It writes a host monotonic
deadline outside the task's agent-log mount. Melissa uses the shorter of that
deadline and any operator `wall_time_sec`/`MELISSA_BENCH_WALL_MS` cap, leaving up
to two seconds for cleanup. Without the plugin, supply `--ak wall_time_sec=N`
and the matching Harbor timeout explicitly; otherwise the adapter refuses to
silently invent the task deadline. An unlimited Harbor timeout retains a bounded
30-minute host run. This does not extend any benchmark limit.

Each request receives the live remaining time as a trailing, request-local
notice. The stable system/history prefix is preserved for cache reuse; earlier
countdown messages do not accumulate. `MELISSA_BENCH_REQUEST_MS` optionally lowers the per-request
timeout (default five minutes). Cutting an already-sent request retains unknown
usage and stops for reconciliation, **not** an automatic replay. This is not a
provider-independent reasoning-effort control.

Output-limit responses are discarded without executing their tool proposals,
then retried with shorter-step guidance at most four times. Proven-not-sent
transport failures can retry. HTTP 429 **does not prove zero usage**: by default
it retains unknown accounting and stops. Set `MELISSA_BENCH_ZERO_USAGE_429=true`
only with a verified provider guarantee of zero usage on those rejections.
The CLI also enables live time guidance and up to two output-limit retries;
it does not enable the zero-usage 429 assertion.

## Interrupted-run evidence

- `melissa-progress.json` in the host trial directory is atomically replaced
  before/after requests and tool dispatch. It contains counts, reported usage,
  pending/unknown counts and phase, not prompts, credentials or model output.
- `agent/melissa-result.json` is also written on timeout, cancellation, process
  failure and spawn failure. Cancellation propagates back to Harbor after child
  cleanup and evidence publication.
- Reported usage is distinct from conservative accounting reservations. If a
  request is unresolved, partial known totals remain lower bounds in metadata;
  Harbor's exact token-total fields are left unset. Stopping a child cannot
  prove that an outstanding container command had no effect.
- The progress sink is opt-in and best-effort. Disk/I/O failure may leave an
  older snapshot; abrupt host loss may lose the latest update. It is neither a
  continuation checkpoint nor an invoice or verifier receipt.

```sh
python3 bench/harbor/summarize.py /path/to/job
python3 -m unittest discover -s tools/tests -p test_melissa_harbor.py
# Run the same command inside an environment with Harbor==0.26.0 to include
# subprocess cancellation/deadline-plugin tests (otherwise those are skipped).
# Optional real non-root Docker transfer test; uses an already-local Linux
# image with GNU tools, never pulls images or calls a model:
# MELISSA_HARBOR_DOCKER_IMAGE=your-local-image python3 -m unittest discover -s tools/tests -p test_melissa_harbor.py
cargo test --locked -p melissa-model-gateway --test agent_loop --test host_run_limit -- --test-threads=1
cargo test --locked -p melissa-bench-agent
```

Summaries include all observed trials, including errors/timeouts; they do not
silently discard provider failures or merge retries into a selectively improved
score. Use the original full task denominator and an identical model/configuration
for comparisons. Provider prices in the bridge are accounting placeholders, not
verified invoice rates. No paid benchmark is run by the tests above.
