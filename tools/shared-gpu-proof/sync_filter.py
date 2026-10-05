"""Compile the native Rust GPU source for portable GPUs and check for drift.

#### PR #22
Generated WGSL is a build artifact, never an independently maintained engine.
CI rebuilds it with the pinned compiler and compares every byte. Use --write
after changing shared Rust source; normal Cargo builds verify its manifests.
Outputs: the 17 T2 filters (reference/shared-t2) and the other portable
stages (reference/shared-stages).
"""
from pathlib import Path
import argparse
import hashlib
import os
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
COMMON = (
    "tools/shared-gpu-proof/Cargo.toml",
    "tools/shared-gpu-proof/Cargo.lock",
    "tools/shared-gpu-proof/rust-toolchain.toml",
    "tools/shared-gpu-proof/builder/Cargo.toml",
    "tools/shared-gpu-proof/builder/src/main.rs",
    "tools/shared-gpu-proof/sync_filter.py",
)
TARGETS = (
    {
        "name": "filter",
        "environment": "PICKAXE_BUILD_SHARED_FILTER",
        "output": "reference/shared-t2",
        "sources": (
            "rust-engine/src/sha256.rs",
            "rust-engine/src/t2_block.rs",
            "tools/shared-gpu-proof/filter/Cargo.toml",
            "tools/shared-gpu-proof/filter/src/lib.rs",
        ),
        "shaders": tuple(f"pickaxe_shared_t2_{i}.wgsl" for i in range(17)),
    },
    {
        "name": "stages",
        "environment": "PICKAXE_BUILD_SHARED_STAGES",
        "output": "reference/shared-stages",
        "sources": (
            "rust-engine/src/field.rs",
            "rust-engine/src/point.rs",
            "rust-engine/src/scalar.rs",
            "rust-engine/src/sha256.rs",
            "rust-engine/src/sign.rs",
            "rust-engine/src/wide.rs",
            "rust-engine/src/window.rs",
            "tools/shared-gpu-proof/stages/Cargo.toml",
            "tools/shared-gpu-proof/stages/src/lib.rs",
        ),
        "shaders": ("pickaxe_shared_stages.wgsl",),
        "loop_copies": {"pickaxe_shared_stages.wgsl": "pickaxe_shared_stages_dx12_metal.wgsl"},
    },
)


def normalized(path):
    return path.read_bytes().replace(b"\r\n", b"\n")


# #### PR #22: loop values for DirectX 12 and Metal
# What: naga's HLSL and MSL writers emit a WGSL `continuing` block at the top
# of the next iteration and recompute any loop-body `let` it uses there, after
# the block's own assignments. A value that loads a variable written later in
# the body, or in the continuing block, then reads the new contents: the
# shared field inverse tested the dummy value it had just stored, never left
# its loop, and the whole stage produced nothing on DirectX 12. A second copy
# of the shared stages copies each such value to a function variable where it
# is computed, and its continuing blocks read the copy; DirectX 12, Metal and
# non-Chromium browsers run that copy. Vulkan and Chromium (Tint) were already
# correct and keep the original shader byte-identical. The T2 filters need no
# copies; generation stops if a shader without a variant ever does.
LET = re.compile(r"^(\s*)let (_e\d+): (.+?) = (.*);$")
VALUE = re.compile(r"\b_e\d+\b")
VARIABLE = re.compile(r"\b(?:phi_\w+|local_\w+|global\w*)\b")
ASSIGN = re.compile(r"^\s*([A-Za-z_]\w*)[^=]*?(?<![=!<>])=(?!=)")


def block_end(lines, start):
    """Returns the index of the line closing the block opened on `start`."""
    depth = 0
    for index in range(start, len(lines)):
        depth += lines[index].count("{") - lines[index].count("}")
        if depth == 0:
            return index
    raise ValueError(f"unbalanced block at line {start + 1}")


def written(lines):
    roots = set()
    for line in lines:
        stripped = line.strip()
        if stripped.startswith(("let ", "var ", "break if", "if ", "} else", "return")):
            continue
        match = ASSIGN.match(line)
        if match:
            roots.add(match.group(1))
    return roots


def capture_loop_values(text):
    """Copies continuing-block values that naga's HLSL/MSL would read late."""
    lines = text.split("\n")
    captures = {}  # function start -> {value: type}
    function = None
    index = 0
    while index < len(lines):
        if re.match(r"^fn \w+\(", lines[index]):
            function = index
        stripped = lines[index].strip()
        if stripped == "loop {":
            indent = len(lines[index]) - len(lines[index].lstrip())
            end = block_end(lines, index)
            cont = next((i for i in range(index + 1, end)
                         if lines[i] == " " * (indent + 4) + "continuing {"), None)
            if cont is not None:
                cont_end = block_end(lines, cont)
                top = " " * (indent + 4)
                lets = {}
                for i in range(index + 1, cont):
                    match = LET.match(lines[i])
                    if match and match.group(1) == top:
                        lets[match.group(2)] = (i, match.group(3), match.group(4))
                later_writes = written(lines[cont + 1:cont_end])

                def hazardous(name, seen=()):
                    line, _, value = lets[name]
                    loads = set(VARIABLE.findall(value))
                    if loads & (written(lines[line + 1:cont]) | later_writes):
                        return True
                    return any(dep in lets and dep not in seen and hazardous(dep, seen + (name,))
                               for dep in VALUE.findall(value))

                used = {name for i in range(cont + 1, cont_end)
                        for name in VALUE.findall(lines[i]) if name in lets}
                for name in sorted(used):
                    if hazardous(name):
                        line, kind, _ = lets[name]
                        captures.setdefault(function, {})[name] = (line, kind, cont, cont_end)
        index += 1
    if not captures:
        return text, 0
    count = 0
    replace = {}
    inserts = {}
    for function, values in captures.items():
        for name, (line, kind, cont, cont_end) in values.items():
            inserts.setdefault(line, []).append(f"{LET.match(lines[line]).group(1)}{name}_late = {name};")
            for i in range(cont + 1, cont_end):
                replace.setdefault(i, set()).add(name)
            count += 1
    out = []
    for i, line in enumerate(lines):
        for name in sorted(replace.get(i, ())):
            line = re.sub(rf"\b{name}\b", f"{name}_late", line)
        out.append(line)
        if i in captures:
            out.extend(f"    var {name}_late: {kind};"
                       for name, (_, kind, _, _) in sorted(captures[i].items()))
        out.extend(inserts.get(i, ()))
    return "\n".join(out), count


def manifest(files):
    return "".join(f"{hashlib.sha256(data).hexdigest()}  {path}\n"
                   for path, data in sorted(files.items()))


def sync(target, write):
    # The fully inlined signing stages exceed rustc's default 8 MiB stack.
    environment = dict(os.environ, RUST_MIN_STACK="536870912", **{target["environment"]: "1"})
    subprocess.run([
        "cargo", "+nightly-2026-04-11", "run", "--locked", "--release",
        "--manifest-path", "tools/shared-gpu-proof/Cargo.toml",
        "-p", "pickaxe-shared-hash-builder",
    ], cwd=ROOT, env=environment, check=True)
    generated = ROOT / "artifacts/shared-gpu-proof" / target["name"]
    output = ROOT / target["output"]
    files = {path: normalized(ROOT / path) for path in target["sources"] + COMMON}
    outputs = {}
    for name in target["shaders"]:
        data = normalized(generated / name)
        outputs[name] = data
        copy, copied = capture_loop_values(data.decode())
        variant = target.get("loop_copies", {}).get(name)
        if variant:
            outputs[variant] = copy.encode()
            print(f"{variant}: copied {copied} loop values for naga's HLSL and MSL writers")
        elif copied:
            raise SystemExit(f"{name} needs {copied} loop copies for DirectX 12 and Metal; "
                             "name a loop_copies variant for it")
    for name, data in outputs.items():
        path = output / name
        if write:
            output.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        elif not path.is_file() or normalized(path) != data:
            raise SystemExit(f"Generated shader drift: {name}; run sync_filter.py --write")
        files[path.relative_to(ROOT).as_posix()] = data
    expected = manifest(files).encode()
    path = output / "SHA256SUMS"
    if write:
        path.write_bytes(expected)
    elif not path.is_file() or normalized(path) != expected:
        raise SystemExit(f"Shared Rust provenance changed ({target['name']}); run sync_filter.py --write")
    print(f"All {len(outputs)} portable {target['name']} shaders match the pinned shared Rust source.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="update generated shaders and provenance")
    args = parser.parse_args()
    for target in TARGETS:
        sync(target, args.write)


if __name__ == "__main__":
    main()
