"""Compare two built miners offline. Stop live mining before running this script."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--output", type=Path, default=Path("artifacts/rust-t2-comparison"))
    args = parser.parse_args()
    bins = {"cpp": args.baseline.resolve(), "rust": args.candidate.resolve()}
    for exe in bins.values():
        if not exe.is_file():
            parser.error(f"Missing executable: {exe}")
    args.output.mkdir(parents=True, exist_ok=True)
    artifacts = {}
    for name, exe in bins.items():
        files = [exe, *sorted((exe.parent / "cuda/build").glob("*.ptx"))]
        artifacts[name] = {
            str(path.relative_to(exe.parent)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in files
        }
    (args.output / "artifacts.json").write_text(json.dumps(artifacts, indent=2), encoding="utf-8")

    def run(name, seconds, label):
        exe = bins[name]
        result = subprocess.run(
            [str(exe), "benchmark", "--backend", "cuda", "--intensity", "100",
             "--seconds", str(seconds), "--json"],
            cwd=exe.parent, capture_output=True, encoding="utf-8", timeout=180,
        )
        (args.output / f"{label}.stdout.json").write_text(result.stdout, encoding="utf-8")
        (args.output / f"{label}.stderr.log").write_text(result.stderr, encoding="utf-8")
        result.check_returncode()
        report = json.loads(result.stdout)
        if report["status"] != "PASS":
            raise RuntimeError(f"Benchmark failed: {label}")
        sample = report["samples"][0]
        print(f"{label}: {sample['candidates_per_second'] / 1e6:.2f} MH/s", flush=True)
        return {"backend": name, "label": label, "report": report}

    run("cpp", 15, "warmup-cpp")
    run("rust", 15, "warmup-rust")
    trials = []
    for i, name in enumerate(["cpp", "rust", "rust", "cpp", "cpp", "rust"], 1):
        trials.append(run(name, 20, f"{i}-{name}"))
        (args.output / "comparison.json").write_text(json.dumps(trials, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
