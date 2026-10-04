"""Compare exported PTX kernels, ignoring comments and source-line annotations only."""
import argparse
import hashlib
import json
import re
from pathlib import Path


def kernels(path):
    source = path.read_text(encoding="utf-8")
    source = re.sub(r"//[^\n]*", "", source)
    result = {}
    for match in re.finditer(r"\.visible\s+\.entry\s+(\w+)\s*\(", source):
        begin = source.index("{", match.end())
        depth = 1
        end = begin + 1
        while depth:
            depth += (source[end] == "{") - (source[end] == "}")
            end += 1
        body = source[match.start():end]
        body = re.sub(r"(?m)^\s*\.loc\s+[^\n]*", "", body)
        result[match.group(1)] = re.sub(r"\s+", " ", body).strip()
    if not result:
        raise ValueError(f"no exported kernels in {path}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    before, after = kernels(args.baseline), kernels(args.candidate)
    changed = [name for name in before if name in after and before[name] != after[name]]
    report = {
        "baseline_sha256": hashlib.sha256(args.baseline.read_bytes()).hexdigest(),
        "candidate_sha256": hashlib.sha256(args.candidate.read_bytes()).hexdigest(),
        "kernels": len(before),
        "identical": sum(before[name] == after.get(name) for name in before),
        "changed": changed,
        "missing": sorted(before.keys() - after.keys()),
        "added": sorted(after.keys() - before.keys()),
        "scope": "exported kernel bodies and declarations; comments/loc/whitespace ignored; not runtime parity",
    }
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    raise SystemExit(bool(changed or report["missing"] or report["added"]))


if __name__ == "__main__":
    main()
