#!/usr/bin/env python3
"""WS9.3(b) measurement harness: REPL-only vs hybrid tool surface.

Runs a fixed suite of coding tasks through `phi` one-shot mode
(`--output ndjson`) once per variant and aggregates success rate, turns,
wall-clock, and the WS9.6 model/exec/dispatch attributions. The variants
differ only in `coding_tools_native` (config.lua), toggled via an isolated
per-variant XDG_CONFIG_HOME so the user's real config and extensions never
leak into a run. Credentials still resolve normally (auth.json lives in the
XDG *data* dir, which is left alone, and `{PROVIDER}_API_KEY` env vars pass
through).

Usage:
    python3 bench/run.py --model openrouter/qwen/qwen3-coder
    python3 bench/run.py --model MODEL --tasks crash-fix,build-cli --reps 3

Success is objective: each task ships a checker (`check.sh`, kept outside
the agent's working directory) that exits 0 iff the task is done, plus an
optional `immutable` file list the runner hash-verifies.
"""

import argparse
import datetime
import hashlib
import json
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
TASKS_DIR = BENCH_DIR / "tasks"
VARIANTS = {"repl": False, "hybrid": True}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_tasks(selection):
    tasks = []
    for task_dir in sorted(TASKS_DIR.iterdir()):
        if not (task_dir / "task.json").is_file():
            continue
        if selection and task_dir.name not in selection:
            continue
        spec = json.loads((task_dir / "task.json").read_text())
        spec["name"] = task_dir.name
        spec["dir"] = task_dir
        tasks.append(spec)
    missing = set(selection or []) - {t["name"] for t in tasks}
    if missing:
        sys.exit(f"unknown task(s): {', '.join(sorted(missing))}")
    if not tasks:
        sys.exit(f"no tasks found under {TASKS_DIR}")
    return tasks


def variant_config_home(out_dir: Path, variant: str) -> Path:
    """A minimal, isolated XDG_CONFIG_HOME for one variant."""
    home = out_dir / f"xdg-{variant}"
    phi_dir = home / "phi"
    phi_dir.mkdir(parents=True, exist_ok=True)
    native = "true" if VARIANTS[variant] else "false"
    (phi_dir / "config.lua").write_text(
        f"return {{ coding_tools_native = {native} }}\n"
    )
    return home


def parse_events(events_path: Path):
    """Fold the NDJSON stream into per-run metrics."""
    metrics = {
        "turns": 0,
        "model_s": 0.0,
        "exec_s": 0.0,
        "dispatch_s": 0.0,
        "repl_blocks": 0,
        "tool_calls": {},
        "result": None,
    }
    for line in events_path.read_text().splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "result":
            metrics["result"] = event
            continue
        # RlmEvent serializes externally tagged: {"IterationEnd": {...}}.
        if "IterationEnd" in event:
            it = event["IterationEnd"]
            if it.get("depth") == 0:
                metrics["turns"] += 1
                metrics["model_s"] += it.get("model", 0.0)
                metrics["exec_s"] += it.get("exec", 0.0)
                metrics["dispatch_s"] += it.get("dispatch", 0.0)
        elif "CodeExecStart" in event:
            if event["CodeExecStart"].get("depth") == 0:
                metrics["repl_blocks"] += 1
        elif "ToolStart" in event:
            tool = event["ToolStart"]
            if tool.get("depth") == 0:
                name = tool.get("name", "?")
                metrics["tool_calls"][name] = metrics["tool_calls"].get(name, 0) + 1
    return metrics


def run_check(task, work_dir: Path) -> tuple[bool, str]:
    for rel in task.get("immutable", []):
        actual = work_dir / rel
        if not actual.is_file():
            return False, f"immutable file deleted: {rel}"
        if sha256(actual) != sha256(task["dir"] / "fixture" / rel):
            return False, f"immutable file modified: {rel}"
    check = task["dir"] / "check.sh"
    proc = subprocess.run(
        ["bash", str(check)],
        cwd=work_dir,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if proc.returncode == 0:
        return True, ""
    detail = (proc.stdout + proc.stderr).strip()
    return False, f"check failed (exit {proc.returncode}): {detail[:500]}"


def run_once(phi, model, args, task, variant, config_home, run_dir: Path, env):
    work = run_dir / "work"
    shutil.copytree(task["dir"] / "fixture", work)
    events_path = run_dir / "events.ndjson"
    stderr_path = run_dir / "stderr.log"

    timeout = task.get("timeout", args.timeout)
    cmd = [phi, "--output", "ndjson", "--timeout", str(timeout), "--model", model]
    if args.sub_model:
        cmd += ["--sub-model", args.sub_model]
    if args.thinking:
        cmd += ["--thinking", args.thinking]
    cmd.append(task["prompt"])

    run_env = dict(env, XDG_CONFIG_HOME=str(config_home))
    record = {"task": task["name"], "variant": variant, "cmd_timeout": timeout}
    try:
        with open(events_path, "w") as out, open(stderr_path, "w") as err:
            proc = subprocess.run(
                cmd,
                cwd=work,
                stdout=out,
                stderr=err,
                stdin=subprocess.DEVNULL,
                timeout=timeout + 120,
                env=run_env,
            )
        record["exit_code"] = proc.returncode
    except subprocess.TimeoutExpired:
        record["exit_code"] = None
        record["error"] = "runner timeout (phi --timeout did not fire)"

    record.update(parse_events(events_path))
    result = record.pop("result", None)
    if result:
        record["stop"] = result.get("stop")
        record["iterations"] = result.get("iterations")
        record["wall_s"] = result.get("duration")
        record["usage"] = result.get("usage")

    if record.get("exit_code") == 0:
        passed, reason = run_check(task, work)
    else:
        passed, reason = False, record.get("error", "phi exited nonzero")
    record["pass"] = passed
    if reason:
        record["fail_reason"] = reason
    return record


def mean(values):
    values = [v for v in values if v is not None]
    return round(statistics.mean(values), 2) if values else None


def summarize(records):
    """(variant, task) -> aggregates, plus per-variant totals."""
    summary = {}
    for rec in records:
        key = (rec["variant"], rec["task"])
        summary.setdefault(key, []).append(rec)
    rows = []
    for (variant, task), recs in sorted(summary.items()):
        rows.append(
            {
                "variant": variant,
                "task": task,
                "runs": len(recs),
                "pass_rate": round(sum(r["pass"] for r in recs) / len(recs), 2),
                "turns": mean([r.get("iterations") for r in recs]),
                "wall_s": mean([r.get("wall_s") for r in recs]),
                "model_s": mean([r.get("model_s") for r in recs]),
                "exec_s": mean([r.get("exec_s") for r in recs]),
                "tokens": mean(
                    [r.get("usage", {}).get("total_tokens") for r in recs if r.get("usage")]
                ),
            }
        )
    return rows


def print_table(rows):
    cols = ["variant", "task", "runs", "pass_rate", "turns", "wall_s", "model_s", "exec_s", "tokens"]
    widths = {c: max(len(c), *(len(str(r[c])) for r in rows)) for c in cols}
    header = "  ".join(c.ljust(widths[c]) for c in cols)
    print(header)
    print("-" * len(header))
    for row in rows:
        print("  ".join(str(row[c]).ljust(widths[c]) for c in cols))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--model", required=True, help="provider/model-id to bench")
    parser.add_argument("--sub-model", help="model for llm_query/subagent sub-calls")
    parser.add_argument("--thinking", help="thinking level (default: phi's default)")
    parser.add_argument("--phi", help="phi binary (default: ./result/bin/phi, else PATH)")
    parser.add_argument("--tasks", help="comma-separated task names (default: all)")
    parser.add_argument("--variants", default="repl,hybrid", help="comma-separated subset of repl,hybrid")
    parser.add_argument("--reps", type=int, default=1, help="repetitions per (task, variant)")
    parser.add_argument("--timeout", type=int, default=300, help="per-run --timeout seconds (task.json overrides)")
    parser.add_argument("--out", help="results dir (default: bench/results/<timestamp>)")
    args = parser.parse_args()

    phi = args.phi or str(
        p if (p := BENCH_DIR.parent / "result" / "bin" / "phi").is_file() else "phi"
    )
    variants = [v.strip() for v in args.variants.split(",") if v.strip()]
    for variant in variants:
        if variant not in VARIANTS:
            sys.exit(f"unknown variant '{variant}' (expected: {', '.join(VARIANTS)})")
    tasks = load_tasks([t.strip() for t in args.tasks.split(",")] if args.tasks else None)

    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    out_dir = Path(args.out) if args.out else BENCH_DIR / "results" / stamp
    out_dir.mkdir(parents=True, exist_ok=True)

    import os

    env = dict(os.environ)
    records = []
    for variant in variants:
        config_home = variant_config_home(out_dir, variant)
        for task in tasks:
            for rep in range(args.reps):
                run_dir = out_dir / variant / task["name"] / f"rep{rep}"
                run_dir.mkdir(parents=True)
                print(f"[{variant}] {task['name']} rep{rep} ...", flush=True)
                rec = run_once(phi, args.model, args, task, variant, config_home, run_dir, env)
                rec["rep"] = rep
                status = "PASS" if rec["pass"] else f"FAIL ({rec.get('fail_reason', '?')})"
                turns = rec.get("iterations", "?")
                wall = rec.get("wall_s")
                wall = f"{wall:.0f}s" if isinstance(wall, (int, float)) else "?"
                print(f"    {status}  turns={turns} wall={wall}", flush=True)
                records.append(rec)

    rows = summarize(records)
    (out_dir / "results.json").write_text(
        json.dumps({"model": args.model, "records": records, "summary": rows}, indent=2)
    )
    print(f"\nmodel: {args.model}   results: {out_dir}/results.json\n")
    print_table(rows)


if __name__ == "__main__":
    main()
