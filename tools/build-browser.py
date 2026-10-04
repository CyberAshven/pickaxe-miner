"""Build the browser client from the same locked Rust crate as the native CLI."""
from pathlib import Path
import shutil
import subprocess
import tomllib
import argparse

ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)


def main():
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    version = next(p["version"] for p in lock["package"] if p["name"] == "wasm-bindgen")
    actual = subprocess.check_output(["wasm-bindgen", "--version"], text=True).strip()
    if actual != f"wasm-bindgen {version}":
        raise SystemExit(f"Install the matching CLI: cargo install --locked wasm-bindgen-cli --version {version}")
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=ROOT / "dist" / "web")
    output = parser.parse_args().out_dir.resolve()
    compiler = ROOT / "web/node_modules/typescript/bin/tsc"
    if not compiler.is_file():
        raise SystemExit("Install browser build tools: npm --prefix web ci --ignore-scripts")
    output.mkdir(parents=True, exist_ok=True)
    run("cargo", "build", "--locked", "--release", "--lib", "--no-default-features",
        "--features", "portable-wgpu", "--target", "wasm32-unknown-unknown")
    metadata = subprocess.check_output(["cargo", "metadata", "--format-version=1", "--no-deps"], cwd=ROOT)
    import json
    target = Path(json.loads(metadata)["target_directory"])
    run("wasm-bindgen", str(target / "wasm32-unknown-unknown/release/pickaxe_miner.wasm"),
        "--target", "web", "--out-dir", str(ROOT / "web/pkg"))
    run("node", str(compiler), "--project", str(ROOT / "web"), "--outDir", str(output))
    shutil.copytree(ROOT / "web/pkg", output / "pkg", dirs_exist_ok=True)
    for name in ("index.html", "style.css"):
        shutil.copyfile(ROOT / "web" / name, output / name)
    shutil.copyfile(ROOT / "LICENSE", output / "LICENSE")
    (output / "SOURCE_COMMIT.txt").write_text(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True))
    run("cargo", "run", "--locked", "--release", "--no-default-features", "--example",
        "browser_table", "--", str(output / "photon-generator-table.bin"))
    print(f"Serve locally: python -m http.server 8080 --bind 127.0.0.1 --directory {output}")


if __name__ == "__main__":
    main()
