#!/usr/bin/env python3
"""Self-test for the task suite: every checker must FAIL on the pristine
fixture and PASS on the committed reference solution (`solution.sh`, run
inside a fixture copy). Run this after adding or editing a task — a checker
that can't pass, or passes with no work done, invalidates the bench.

    python3 bench/validate.py
"""

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent


def run_check(task_dir: Path, work: Path) -> bool:
    proc = subprocess.run(
        ["bash", str(task_dir / "check.sh")],
        cwd=work,
        capture_output=True,
        text=True,
        timeout=120,
    )
    return proc.returncode == 0


def main():
    failures = []
    for task_dir in sorted((BENCH_DIR / "tasks").iterdir()):
        if not (task_dir / "task.json").is_file():
            continue
        name = task_dir.name
        with tempfile.TemporaryDirectory() as tmp:
            pristine = Path(tmp) / "pristine"
            shutil.copytree(task_dir / "fixture", pristine)
            if run_check(task_dir, pristine):
                failures.append(f"{name}: check PASSES on the pristine fixture")

            solved = Path(tmp) / "solved"
            shutil.copytree(task_dir / "fixture", solved)
            solution = task_dir / "solution.sh"
            if not solution.is_file():
                failures.append(f"{name}: no solution.sh reference solution")
                continue
            proc = subprocess.run(
                ["bash", str(solution)],
                cwd=solved,
                capture_output=True,
                text=True,
                timeout=120,
            )
            if proc.returncode != 0:
                failures.append(f"{name}: solution.sh failed: {proc.stderr[:200]}")
            elif not run_check(task_dir, solved):
                failures.append(f"{name}: check FAILS on the reference solution")
        print(f"{name}: {'FAIL' if any(f.startswith(name + ':') for f in failures) else 'ok'}")

    if failures:
        print("\n" + "\n".join(failures))
        sys.exit(1)
    print("\nall task checkers validated")


if __name__ == "__main__":
    main()
