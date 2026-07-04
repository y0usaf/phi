# bench — WS9.3(b) tool-surface measurement harness

Compares the two tool surfaces on a fixed coding-task suite, per PLAN.md
WS9.3(b): enabling `coding-tools-native` (the hybrid surface) by default is
gated on measurement — *no data, no default change*.

- **repl** — `coding_tools_native = false` (today's default): the model gets
  the single `repl` tool and composes the prelude helpers in Python.
- **hybrid** — `coding_tools_native = true`: bash/read_file/edit_file/
  write_file/grep/find/ls additionally surface as native provider tools
  (`expose: both`, same prelude implementations underneath).

## Run

```bash
nix build .#phi-app          # the runner defaults to ./result/bin/phi
python3 bench/run.py --model openrouter/qwen/qwen3-coder
python3 bench/run.py --model MODEL --tasks crash-fix,build-cli --reps 3
```

Requires credentials for the chosen provider (`{PROVIDER}_API_KEY` env var
or `phi --login`). Each run is isolated: a per-variant `XDG_CONFIG_HOME`
carries only the `coding_tools_native` toggle, so your real config.lua and
extensions never affect results; the XDG data dir (auth.json) is untouched.

Results land in `bench/results/<timestamp>/` (git-ignored): per-run
`events.ndjson` + working tree + `stderr.log`, plus `results.json` and a
printed summary table (pass rate, mean turns, wall-clock, and the WS9.6
model/exec attributions, tokens).

## Tasks

Each `tasks/<name>/` has `task.json` (prompt, timeout, `immutable` file
list), a `fixture/` copied to a fresh working dir per run, and a `check.sh`
that exits 0 iff the task succeeded. Checkers live *outside* the working
dir (the agent never sees them) and the runner hash-verifies `immutable`
files, so results can't be gamed by editing tests. `tasks/README.md`-level
principle: objective, deterministic, stdlib-only checks — no network, no
toolchains beyond python3 + coreutils.

| task | exercises |
|---|---|
| fix-failing-test | run tests → localize bug → minimal fix, tests immutable |
| rename-across-files | project-wide grep + consistent multi-file edit |
| grep-and-fix | symptom → search → off-by-one fix in a distractor field |
| build-cli | write a new script to a precise output spec |
| json-transform | read/transform/write structured data |
| changelog-insert | precise positional markdown editing (hashline territory) |
| config-sweep | loop-edit many files with a per-file condition |
| crash-fix | reproduce a crash → fix with a specified fallback semantic |

## Reading the results

The decision metric order is: pass rate, then turns, then wall-clock
(model-time dominates; exec/dispatch attribute the remainder). Turns and
wall-clock only mean something between runs that *passed*. For a default
change, run the full suite with `--reps ≥ 3` on at least two models you
actually use daily and compare variants per model — a hybrid win on one
model is not evidence for a default that ships to every provider.
