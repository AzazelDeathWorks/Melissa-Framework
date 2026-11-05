#!/usr/bin/env python3
"""Materialize a deterministic synthetic workload for Harness Lab."""

import argparse
import random
from dataclasses import dataclass
from pathlib import Path

from harness_lab import write


@dataclass(frozen=True)
class Operation:
    description: str
    python: str
    rust: str
    reference: object


OPERATIONS = (
    Operation("add {n}", "x + {n}", "x + {n}", lambda x, n: x + n),
    Operation("subtract {n}", "x - {n}", "x - {n}", lambda x, n: x - n),
    Operation("multiply by {n}", "x * {n}", "x * {n}", lambda x, n: x * n),
    Operation("take the nonnegative remainder modulo {n}", "x % {n}", "x.rem_euclid({n})",
              lambda x, n: divmod(x, n)[1]),
    Operation("round down after dividing by {n}", "x // {n}", "x.div_euclid({n})",
              lambda x, n: divmod(x, n)[0]),
    Operation("clamp between -{n} and {n}", "max(-{n}, min({n}, x))", "x.clamp(-{n}, {n})",
              lambda x, n: -n if x < -n else n if x > n else x),
    Operation("reflect around {n}", "2 * {n} - x", "2 * {n} - x", lambda x, n: n - (x - n)),
    Operation("take distance from {n}", "abs(x - {n})", "(x - {n}).abs()",
              lambda x, n: x - n if x >= n else n - x),
    Operation("keep at least {n}", "max({n}, x)", "x.max({n})", lambda x, n: x if x >= n else n),
    Operation("round down to a multiple of {n}", "x - x % {n}", "x - x.rem_euclid({n})",
              lambda x, n: divmod(x, n)[0] * n),
)


def save(root, name, content):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as out:
        out.write(content)


def compute(value, steps, broken=()):
    for index, (op, amount) in enumerate(steps):
        value = value + 1 if index in broken else op.reference(value, amount)
    return value


def cases(rng, steps, broken):
    values = list(range(-201, 202))
    rng.shuffle(values)
    different = [value for value in values
                 if compute(value, steps) != compute(value, steps, broken)]
    if len(different) < 4:
        return None
    selected = different[:4] + [value for value in values if value not in different[:4]][:4]
    return [
        [{"id": f"{kind}-{offset}", "input": value, "expected": compute(value, steps)}
         for offset, value in enumerate(inputs)]
        for kind, inputs in (("public", selected[:2]), ("protected", selected[2:]))
    ]


def python_tree(source, negative, steps, broken):
    imports = ["import json", "import sys"]
    runner = "print(json.dumps(run(int(sys.argv[1]))))\n"
    if len(steps) == 1:
        op, amount = steps[0]
        expression = "x + 1" if 0 in broken else op.python.format(n=amount)
        save(source, "main.py", "\n".join(imports) +
             f"\n\n\ndef run(x: int) -> int:\n    return {expression}\n\n\n" + runner)
        writable = ["main.py"]
    else:
        imports.append("")
        chain, writable = ["def run(x: int) -> int:"], []
        for index, (op, amount) in enumerate(steps):
            stem = f"stage_{index:02d}"
            imports.append(f"from {stem} import run as {stem}")
            chain.append(f"    x = {stem}(x)")
            expression = "x + 1" if index in broken else op.python.format(n=amount)
            save(source, stem + ".py", f"def run(x: int) -> int:\n    return {expression}\n")
            if index in broken:
                writable.append(stem + ".py")
        chain.append("    return x")
        save(source, "main.py", "\n".join(imports + ["", ""] + chain) + "\n\n\n" + runner)
    save(negative, "main.py", 'print("null")\n')
    return writable, "python", "main.py"


def cargo_tree(source, negative, steps, broken):
    names = [f"stage_{index:02d}" for index in range(len(steps))]
    writable = [f"{names[index]}/src/lib.rs" for index in broken]
    manifest = '[workspace]\nresolver = "2"\nmembers = ["app"]\n'
    app = '[package]\nname = "lab-app"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\n'
    dependencies = "".join(f'{name} = {{ path = "../{name}" }}\n' for name in names)
    lock = "version = 3\n\n[[package]]\nname = \"lab-app\"\nversion = \"0.1.0\"\ndependencies = [\n"
    lock += "".join(f' "{name}",\n' for name in names) + "]\n"
    for index, (name, (op, amount)) in enumerate(zip(names, steps)):
        save(source, f"{name}/Cargo.toml", app.split("\n\n[dependencies]")[0].replace("lab-app", name))
        expression = "x + 1" if index in broken else op.rust.format(n=amount)
        save(source, f"{name}/src/lib.rs", f"pub fn run(x: i64) -> i64 {{ {expression} }}\n")
        lock += f'\n[[package]]\nname = "{name}"\nversion = "0.1.0"\n'
    save(source, "Cargo.toml", manifest)
    save(source, "Cargo.lock", lock)
    save(source, "app/Cargo.toml", app + dependencies)
    chain = "".join(f"    x = {name}::run(x);\n" for name in names)
    save(source, "app/src/main.rs", 'fn main() {\n    let mut x = std::env::args().nth(1)'
         '.unwrap().parse::<i64>().unwrap();\n' + chain + '    println!("{x}");\n}\n')
    save(negative, "Cargo.toml", manifest)
    save(negative, "Cargo.lock", 'version = 3\n\n[[package]]\nname = "lab-app"\nversion = "0.1.0"\n')
    save(negative, "app/Cargo.toml", app)
    save(negative, "app/src/main.rs", 'fn main() { println!("null"); }\n')
    return writable, "rust_cargo", "lab-app"


def make_case(output, cohort, index, width, fixes, rng, cost):
    key = f"{cohort.replace('_', '-')}-{index:02d}"
    relative = Path(key)
    case_dir = output / relative
    source, negative = case_dir / "source", case_dir / "negative"
    for _ in range(512):
        ops = OPERATIONS if width <= 8 else OPERATIONS[:6] + OPERATIONS[6:9]
        steps = [(ops[index % len(ops)] if width == 1 else rng.choice(ops), rng.randint(2, 11))
                 for _ in range(width)]
        # A reduction early in a large workspace prevents incidental overflow.
        if width > 8:
            for position in range(7, width, 8):
                steps[position] = (OPERATIONS[3], 1009)
        broken = sorted(rng.sample(range(width), fixes))
        checks = cases(rng, steps, broken)
        if checks is not None:
            break
    else:
        raise ValueError("could not build an observable mutation")
    build = cargo_tree if cohort == "large_rust" else python_tree
    writable, language, entry = build(source, negative, steps, broken)
    requirements = "\n".join(f"{i + 1:02d}. {op.description.format(n=n)}."
                             for i, (op, n) in enumerate(steps))
    save(source, "PIPELINE.md", "# Integer pipeline\n\n" + requirements + "\n")
    goal = ("Repair the integer pipeline to match PIPELINE.md for signed inputs. Keep the "
            "JSON integer CLI output and protected stages. Use the approved public tests.")
    if width == 1:
        goal += " " + steps[0][0].description.format(n=steps[0][1]).capitalize() + "."
    write(case_dir / "recipe.private.json", {
        "schema_version": "melissa.cli-recipe/v2",
        "agents": [{"profile": "melissa.code.implementer", "goal": goal, "write": True}],
        "writable_paths": writable, "protected_paths": ["PIPELINE.md"],
        "negative_workspace": str(negative), "max_cost_micro_usd": cost,
        "wall_time_ms": 7_200_000 if cohort == "long_horizon" else 1_800_000,
        "language": language, "entry": entry, "development_checks": checks[0], "checks": checks[1],
    })
    return {"id": key, "cohort": cohort, "goal": goal, "source": (relative / "source").as_posix(),
            "negative": (relative / "negative").as_posix(),
            "recipe": (relative / "recipe.private.json").as_posix()}


def generate(output, seed, cost):
    output.mkdir()
    rng = random.Random(seed)
    workloads = (("small", 50, 1, 1), ("repository", 50, 8, 1),
                 ("long_horizon", 30, 18, 3), ("large_rust", 20, 40, 4))
    result = [make_case(output, cohort, index, width, faults, rng, cost)
              for cohort, count, width, faults in workloads for index in range(count)]
    write(output / "cases.private.json", {"cases": result, "seed": seed,
                                           "provenance": "generated_synthetic_pipeline_fixtures"})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--seed", type=int, required=True)
    parser.add_argument("--max-case-micro-usd", type=int, required=True)
    args = parser.parse_args()
    if args.max_case_micro_usd < 1:
        parser.error("max-case-micro-usd must be positive")
    generate(args.output.absolute(), args.seed, args.max_case_micro_usd)


if __name__ == "__main__":
    main()
