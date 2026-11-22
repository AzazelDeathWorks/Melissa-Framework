"""Host-only time and telemetry helpers; no provider or Docker dependencies."""
import json
import math
import os
import tempfile
from pathlib import Path


def atomic_json(path: Path, value: dict) -> None:
    fd, name = tempfile.mkstemp(prefix=f".{path.name}-", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(value, f, indent=2)
            f.flush()
            os.fsync(f.fileno())
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


def remaining_wall_ms(budget: dict | None, configured_sec: float | None, now: float) -> int:
    deadline = budget.get("deadline_monotonic") if budget is not None else None
    if configured_sec is not None and (not math.isfinite(configured_sec) or configured_sec <= 0):
        raise ValueError("wall_time_sec must be positive and finite")
    if deadline is not None and not math.isfinite(deadline):
        raise ValueError("invalid Harbor deadline")
    remaining = None if deadline is None else deadline - now
    if configured_sec is not None:
        remaining = min(remaining, configured_sec) if remaining is not None else configured_sec
    if remaining is None:
        if budget is None:
            raise ValueError("use MelissaDeadlinePlugin or set a matching wall_time_sec explicitly")
        remaining = 1800  # Harbor has no deadline; retain a bounded host run.
    if remaining <= 0:
        raise ValueError("Harbor deadline already expired")
    milliseconds = math.floor(remaining * 1000)
    # Finish accounting before Harbor cancels the whole agent coroutine.
    return max(1, milliseconds - min(2000, max(1, milliseconds // 20)))


def update_context(context, done: dict) -> None:
    complete = done.get("telemetry_complete", False) and done.get("unreported_usage_requests", 0) == 0
    # Reservations and partial lower bounds are not actual provider usage.
    context.n_input_tokens = done.get("reported_input_tokens") if complete else None
    context.n_output_tokens = done.get("reported_output_tokens") if complete else None
    context.metadata = {k: done.get(k) for k in (
        "status", "requests", "tool_calls", "elapsed_ms", "wall_ms",
        "reported_input_tokens", "reported_output_tokens", "cached_input_tokens",
        "telemetry_complete", "unreported_usage_requests", "pending_requests",
        "unknown_requests", "tool_in_flight", "requires_reconciliation", "phase", "error_type",
    )}
