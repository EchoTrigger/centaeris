"""Bounded A/B execution of the shared production-runtime acceptance example.

The external Cargo manifest is disposable compilation glue. The baseline source
tree is never changed, and both binaries compile the exact same example source.
No model requests run unless quality --execute --real is explicitly selected.
"""
from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

CASES = (
    "normal-answer", "normal-history", "normal-tool", "compact-goal", "compact-path",
    "compact-failure", "repeat-constraints", "repeat-progress", "repeat-original",
    "tool-chain", "steer-owner", "reopen-context",
)
SCHEMA = "runtime.working_set.result.v1"


def percentile(values, fraction):
    ordered = sorted(values)
    if not ordered:
        raise ValueError("empty measurement group")
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def validate_sample(sample):
    """Recheck task evidence independently of the executable's passed flag."""
    errors = []
    try:
        actual = json.loads(sample.get("reply", ""))
    except (ValueError, TypeError):
        actual = None
    if actual != sample.get("expected"):
        errors.append("task_result_mismatch")
    if sample.get("stop") != "finalized" or sample.get("error"):
        errors.append("runtime_not_finalized")
    case = sample.get("case")
    needs_read = sample.get("mode") == "perf" or case in {"normal-tool", "tool-chain", "steer-owner"}
    if sample.get("proofReads") != int(needs_read):
        errors.append("tool_read_count_mismatch")
    expected_compactions = 2 if str(case).startswith("repeat-") else int(str(case).startswith("compact-") or case == "reopen-context")
    if sample.get("compactions") != [True] * expected_compactions:
        errors.append("compaction_count_or_outcome_mismatch")
    if sample.get("summaryCalls") != expected_compactions:
        errors.append("summary_dispatch_count_mismatch")
    if case == "steer-owner" and (sample.get("mainCalls", 0) < 2 or not sample.get("steerInputObserved")):
        errors.append("correction_not_observed_at_model_boundary")
    if not 0 <= sample.get("mainCalls", -1) <= 6 or not 0 <= sample.get("summaryCalls", -1) <= 2:
        errors.append("model_dispatch_limit_exceeded")
    for field in ("elapsedMs", "firstRequestPreparedMs", "costUpperUsd"):
        value = sample.get(field)
        if not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
            errors.append(f"invalid_{field}")
    return errors


def manifest(root, harness):
    def path(value):
        return json.dumps(str(value.resolve()).replace("\\", "/"))
    return f'''[package]
name = "centaeris-working-set-eval"
version = "0.0.0"
edition = "2021"
[workspace]
[[bin]]
name = "runtime-working-set-eval"
path = {path(harness)}
[dependencies]
centaeris-core = {{ path = {path(root / "packages/core")} }}
centaeris-runtime-sqlite = {{ path = {path(root / "packages/sqlite")} }}
centaeris-model-http = {{ path = {path(root / "packages/model-http")} }}
serde_json = "=1.0.150"
tokio = {{ version = "=1.52.3", features = ["macros", "rt", "time", "net"] }}
'''


def build(args):
    candidate = args.candidate.resolve()
    harness = candidate / "packages/sqlite/examples/runtime_working_set.rs"
    args.output.mkdir(parents=True, exist_ok=True)
    target = args.target.resolve()
    results = {}
    for label, root in (("baseline", args.baseline), ("candidate", candidate)):
        with tempfile.TemporaryDirectory(prefix="centaeris-working-set-build-") as folder:
            wrapper = Path(folder)
            (wrapper / "Cargo.toml").write_text(manifest(root, harness), encoding="utf-8")
            shutil.copyfile(root / "Cargo.lock", wrapper / "Cargo.lock")
            env = dict(os.environ, CARGO_TARGET_DIR=str(target))
            subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(wrapper / "Cargo.toml")], env=env, check=True)
            filename = "runtime-working-set-eval" + (".exe" if os.name == "nt" else "")
            destination = args.output / (label + (".exe" if os.name == "nt" else ""))
            shutil.copyfile(target / "release" / filename, destination)
            results[label] = str(destination.resolve())
    print(json.dumps({"schema": "runtime.working_set.binaries.v1", "binaries": results}))


def invoke(binary, command):
    result = subprocess.run([str(binary.resolve())], input=json.dumps(command).encode("utf-8"),
                            capture_output=True, timeout=2400, check=True)
    try:
        lines = [line for line in result.stdout.decode("utf-8").splitlines() if line.strip()]
        if len(lines) != 1:
            raise ValueError("executable must emit exactly one JSON receipt")
        value = json.loads(lines[0])
        if value.get("schema") != SCHEMA or not isinstance(value.get("samples"), list):
            raise ValueError("invalid runtime acceptance receipt")
    except (UnicodeDecodeError, ValueError) as error:
        root = Path(command["root"])
        root.mkdir(parents=True, exist_ok=True)
        (root / "rejected-evaluation-receipt.bin").write_bytes(result.stdout)
        raise ValueError("invalid evaluator UTF-8 JSON receipt; raw bytes retained") from error
    return value


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    pending = path.with_suffix(path.suffix + ".pending")
    pending.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(pending, path)


def performance(args):
    plan = {"schema": "runtime.working_set.performance.v1", "backend": "sqlite",
            "history": [1000, 10000], "warmup": 20, "samples": 100, "concurrency": 1,
            "groups": [], "executed": args.execute}
    if not args.execute:
        print(json.dumps(plan))
        return
    if args.output.exists():
        raise ValueError("result already exists; inspect its receipts before another campaign")
    root = args.output.parent / (args.output.stem + "-data")
    for history in (1000, 10000):
        # Alternate version order so one arm does not always receive colder caches.
        labels = ("baseline", "candidate") if history == 1000 else ("candidate", "baseline")
        for label in labels:
            command = {"mode": "perf", "history": history, "root": str(root / label / str(history)),
                       "workspaceRoot": str(root / "fixture"), "warmup": 20, "samples": 100}
            receipt = invoke(getattr(args, label), command)
            samples = [sample for sample in receipt["samples"] if not sample.get("warmup")]
            errors = [validate_sample(sample) for sample in samples]
            measured = [sample["firstRequestPreparedMs"] for sample in samples if isinstance(sample.get("firstRequestPreparedMs"), (int, float))]
            group = {"version": label, "history": history, "receipt": receipt, "validation": errors}
            if measured:
                group.update(p50Ms=percentile(measured, .5), p95Ms=percentile(measured, .95))
            group["projectionsPerSample"] = [sum(
                stage.get("stage") == "Projection" and stage.get("completed") is True
                for stage in sample.get("stages", [])) for sample in samples]
            group["oneProjectionPerModelRequest"] = all(
                count == sample.get("mainCalls")
                for count, sample in zip(group["projectionsPerSample"], samples))
            for field in ("retainedSourceBytes", "retainedSourceMessages", "activeMessages"):
                values = [sample[field] for sample in samples if isinstance(sample.get(field), int)]
                group[field + "Max"] = max(values) if values else None
            group["passed"] = len(samples) == 100 and not receipt.get("error") and not any(errors)
            if label == "candidate":
                group["passed"] = group["passed"] and group["oneProjectionPerModelRequest"]
            plan["groups"].append(group)
            save(args.output, plan)
    for label in ("baseline", "candidate"):
        groups = {group["history"]: group for group in plan["groups"] if group["version"] == label}
        ratio = groups[10000].get("p95Ms", math.inf) / max(groups[1000].get("p95Ms", 0), 1e-9)
        plan[label + "HistoryAmplification"] = ratio if math.isfinite(ratio) else None
    plan["passed"] = all(group["passed"] for group in plan["groups"]) and plan["candidateHistoryAmplification"] is not None and plan["candidateHistoryAmplification"] <= 1.2
    save(args.output, plan)
    print(json.dumps({key: value for key, value in plan.items() if key != "groups"}))


def quality(args):
    cases = tuple(dict.fromkeys(args.cases))
    versions = tuple(dict.fromkeys(args.versions))
    if args.repeats < 1 or not math.isfinite(args.max_cost_usd) or args.max_cost_usd <= 0:
        raise ValueError("positive repeat count and finite cost budget required")
    target = len(cases) * args.repeats
    primary_runs = target * len(versions)
    reserve = 12 if set(cases) == set(CASES) and set(versions) == {"baseline", "candidate"} and args.repeats == 2 else 0
    plan = {"schema": "runtime.working_set.quality.v1", "cases": list(cases), "repeats": args.repeats,
            "versions": list(versions), "fullCaseCoverage": set(cases) == set(CASES),
            "targetPerVersion": target, "primaryRuns": primary_runs,
            "maxRuns": primary_runs + reserve, "reserveRuns": reserve, "maxCostUsd": args.max_cost_usd, "runs": [], "executed": args.execute,
            "real": args.real, "costUpperUsd": 0.0}
    rerun = tuple(dict.fromkeys(args.rerun_cases or ()))
    if len(rerun) > 6:
        raise ValueError("affected recheck is limited to six cases / twelve Runs")
    if not args.execute:
        if rerun:
            plan["affectedRecheck"] = list(rerun)
        print(json.dumps(plan))
        return
    if rerun:
        if not args.reason or not args.output.exists():
            raise ValueError("affected recheck requires existing receipts and a concrete change reason")
        plan = json.loads(args.output.read_text(encoding="utf-8"))
        if plan.get("schema") != "runtime.working_set.quality.v1" or plan.get("real") != args.real or plan.get("stopped"):
            raise ValueError("incompatible or unresolved quality receipts")
        primary_scope = {(version, case, repeat) for version in ("baseline", "candidate")
                         for case in CASES for repeat in range(2)}
        recorded_scope = {(run["version"], run["case"], run["repeat"]) for run in plan["runs"]}
        if (any(run.get("recheck") for run in plan["runs"]) or len(plan["runs"]) != 48
                or recorded_scope != primary_scope or plan["repeats"] != 2
                or plan["reserveRuns"] != 12 or plan["maxRuns"] != 60):
            raise ValueError("only one affected recheck is allowed after the complete primary comparison")
        cases = tuple(plan["cases"])
        versions = tuple(dict.fromkeys(run["version"] for run in plan["runs"]))
        target = len(cases) * plan["repeats"]
    elif args.output.exists():
        raise ValueError("result already exists; inspect paid and unknown receipts before rerunning")
    if args.real and (not os.environ.get("COMMAND_CODE_API_KEY") or not all(
            math.isfinite(value) and value > 0 for value in (args.input_rate, args.output_rate))):
        raise ValueError("real evaluation requires injected credential and verified positive input/output rates")
    root = args.output.parent / (args.output.stem + "-data")
    save(args.output, plan)
    jobs = []
    for repeat in range(plan["repeats"]):
        for number, case in enumerate(rerun or cases):
            labels = ("candidate",) if rerun else (versions if (repeat + number) % 2 == 0 else tuple(reversed(versions)))
            jobs.extend((repeat, case, label) for label in labels)
    for repeat, case, label in jobs:
        remaining = plan["maxCostUsd"] - plan["costUpperUsd"]
        if remaining <= 0:
            plan["stopped"] = "cost_limit"
            save(args.output, plan)
            return
        run = {"version": label, "case": case, "repeat": repeat, "status": "dispatching",
               "recheck": bool(rerun), "changeReason": args.reason if rerun else None}
        plan["runs"].append(run)
        save(args.output, plan)  # A killed coordinator leaves a visible unknown dispatch.
        command = {"mode": "quality", "case": case, "real": args.real, "root": str(root / ("recheck" if rerun else "primary") / label / case / str(repeat)),
                   "workspaceRoot": str(root / "fixture"), "warmup": 0, "samples": 1,
                   "budgetUsd": remaining, "inputUsdPerMillion": args.input_rate,
                   "outputUsdPerMillion": args.output_rate}
        try:
            receipt = invoke(getattr(args, label), command)
            if receipt.get("error") or len(receipt["samples"]) != 1:
                raise ValueError("no complete cost receipt after dispatch")
            sample = receipt["samples"][0]
            errors = validate_sample(sample)
            cost = sample["costUpperUsd"] if args.real else 0.0
            if cost > remaining + 1e-8:
                raise ValueError("cost bound violated")
            plan["costUpperUsd"] += cost
            run.update(status="completed", receipt=receipt, validation=errors, passed=not errors)
        except (ValueError, subprocess.SubprocessError) as error:
            # Unknown paid work is not silently repeated or treated as free.
            run.update(status="unknown", passed=False, errorType=type(error).__name__)
            plan["stopped"] = "unknown_dispatch_receipt"
            if args.real:
                plan["costUpperUsd"] = plan["maxCostUsd"]
            save(args.output, plan)
            print(json.dumps({"stopped": plan["stopped"], "runs": len(plan["runs"])}))
            return
        save(args.output, plan)
    effective = {(run["version"], run["case"], run["repeat"]): run for run in plan["runs"]}
    counts = {label: sum(run["passed"] for (version, _, _), run in effective.items() if version == label) for label in versions}
    plan.update(successCounts=counts, completedRuns=len(plan["runs"]),
                passed=all(count == target for count in counts.values()) if len(versions) == 1 else
                    counts["candidate"] == target and counts["candidate"] >= counts["baseline"])
    save(args.output, plan)
    print(json.dumps({key: value for key, value in plan.items() if key != "runs"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    compile_parser = sub.add_parser("build")
    compile_parser.add_argument("--baseline", type=Path, required=True)
    compile_parser.add_argument("--candidate", type=Path, required=True)
    compile_parser.add_argument("--target", type=Path, required=True)
    compile_parser.add_argument("--output", type=Path, required=True)
    for name in ("perf", "quality"):
        command = sub.add_parser(name)
        command.add_argument("--baseline", type=Path, required=True)
        command.add_argument("--candidate", type=Path, required=True)
        command.add_argument("--output", type=Path, required=True)
        command.add_argument("--execute", action="store_true")
        if name == "quality":
            command.add_argument("--real", action="store_true")
            command.add_argument("--input-rate", type=float, default=0.0)
            command.add_argument("--output-rate", type=float, default=0.0)
            command.add_argument("--versions", choices=("baseline", "candidate"), nargs="+", default=["baseline", "candidate"])
            command.add_argument("--cases", choices=CASES, nargs="+", default=list(CASES))
            command.add_argument("--repeats", type=int, default=2)
            command.add_argument("--max-cost-usd", type=float, default=20.0)
            command.add_argument("--rerun-cases", choices=CASES, nargs="+")
            command.add_argument("--reason")
    args = parser.parse_args()
    {"build": build, "perf": performance, "quality": quality}[args.command](args)


if __name__ == "__main__":
    main()
