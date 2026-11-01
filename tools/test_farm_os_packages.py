#!/usr/bin/env python3
"""#### PR #42: the farm-OS packages, built from a fake Linux archive with a
stub pickaxe that records its arguments. Standard library only; the shell
scripts run with bash, the RaveOS scripts with a fake ravinos module."""

import importlib.util
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import types
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "tools" / "farm-os" / "build.sh"
BASH = shutil.which("bash")

STUB = r"""#!/usr/bin/env bash
printf '%s\0' "$@" > "${PICKAXE_ARGV_FILE:-/dev/null}"
if [[ "$1 $2" == "farm-os stats" ]]; then
  case "$4" in
    hiveos) printf '1.500\n{"hs":[1500],"hs_units":"hs","uptime":5}\n' ;;
    mmpos) printf '{"busid":[1],"hash":[1500],"units":"hs"}\n' ;;
    raveos) printf '{"gpus":[{"pci_bus":1,"hash_rate":1500,"temp":61,"fan":40,"accepted":3,"rejected":1}],"accepted":3,"rejected":1,"invalid":0}\n' ;;
  esac
fi
"""


def version():
    for line in (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        if line.startswith("version = "):
            return line.split('"')[1].replace("-", "_")
    raise AssertionError("no version")


def recorded(path):
    return Path(path).read_bytes().decode().split("\0")[:-1]


@unittest.skipIf(BASH is None, "needs bash")
class FarmOsPackages(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp())
        stem = "pickaxe-miner-v0.0.0-linux-x86_64"
        archive = cls.tmp / f"{stem}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for name, data, mode in [
                ("pickaxe", STUB.encode(), 0o755),
                ("cuda/build/photon_rust.ptx", b".target sm_120\n", 0o644),
                ("BACKENDS.txt", b"backends\n", 0o644),
                ("README.md", b"readme\n", 0o644),
                ("LICENSE", b"license\n", 0o644),
            ]:
                info = tarfile.TarInfo(f"{stem}/{name}")
                info.size = len(data)
                info.mode = mode
                tar.addfile(info, io.BytesIO(data))
        cls.out = cls.tmp / "out"
        subprocess.run([BASH, str(BUILD), str(archive), str(cls.out)], check=True,
                       stdout=subprocess.DEVNULL, env=dict(os.environ, PYTHON=sys.executable))
        cls.version = version()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def unpack(self, name):
        target = Path(tempfile.mkdtemp(dir=self.tmp))
        path = self.out / name
        if name.endswith(".zip"):
            with zipfile.ZipFile(path) as package:
                package.extractall(target)
                for info in package.infolist():
                    mode = (info.external_attr >> 16) & 0o777
                    if mode:
                        os.chmod(target / info.filename, mode)
        else:
            with tarfile.open(path) as package:
                if hasattr(tarfile, "data_filter"):
                    package.extractall(target, filter="data")
                else:
                    package.extractall(target)
        return target

    def test_asset_names_and_hiveos_naming_rule(self):
        v = self.version
        self.assertEqual(
            sorted(p.name for p in self.out.iterdir()),
            sorted([f"pickaxe-hiveos-{v}.tar.gz", f"pickaxe-mmpos-{v}.tar.gz",
                    f"pickaxe-raveos-{v}.zip"]),
        )
        for os_name in ("hiveos", "mmpos"):
            name = f"pickaxe-{os_name}-{v}.tar.gz"
            # custom-get: the name minus its last "-" field is the folder.
            folder = name[: -len(".tar.gz")].rsplit("-", 1)[0]
            with tarfile.open(self.out / name) as package:
                tops = {member.name.split("/")[0] for member in package.getmembers()}
            self.assertEqual(tops, {folder})

    def test_files_modes_and_line_endings(self):
        expected = {
            "hiveos": ["h-manifest.conf", "h-config.sh", "h-run.sh", "h-stats.sh"],
            "mmpos": ["mmp-external.conf", "mmp-launch.sh", "mmp-stats.sh"],
        }
        for os_name, files in expected.items():
            package = f"pickaxe-{os_name}-{self.version}.tar.gz"
            folder = self.unpack(package) / f"pickaxe-{os_name}"
            with tarfile.open(self.out / package) as tar:
                modes = {member.name: member.mode for member in tar.getmembers()}
            for name in files + ["pickaxe", "BACKENDS.txt", "LICENSE", "cuda/build/photon_rust.ptx"]:
                path = folder / name
                self.assertTrue(path.is_file(), path)
                self.assertNotIn(b"\r", path.read_bytes(), path)
                executable = name.endswith(".sh") or name == "pickaxe"
                mode = modes[f"pickaxe-{os_name}/{name}"]
                self.assertEqual(bool(mode & stat.S_IXUSR), executable, (name, oct(mode)))
            text = (folder / files[0]).read_text()
            self.assertIn(self.version, text)
            self.assertNotIn("@VERSION@", text)
        rave = self.unpack(f"pickaxe-raveos-{self.version}.zip")
        manifest = json.loads((rave / "RAVINOS" / "manifest.json").read_text())
        self.assertEqual(manifest["package"], f"pickaxe-raveos-{self.version}")
        self.assertEqual(manifest["executable"], ["pickaxe"])
        for name in ("start.py", "stats.py"):
            self.assertNotIn(b"\r", (rave / "RAVINOS" / name).read_bytes())
        with zipfile.ZipFile(self.out / f"pickaxe-raveos-{self.version}.zip") as package:
            modes = {info.filename: info.external_attr >> 16 for info in package.infolist()}
        self.assertEqual(modes["pickaxe"] & 0o777, 0o755)
        self.assertEqual(modes["RAVINOS/start.py"] & 0o777, 0o755)
        self.assertEqual(modes["RAVINOS/manifest.json"] & 0o777, 0o644)

    def test_hiveos_config_run_and_stats(self):
        folder = self.unpack(f"pickaxe-hiveos-{self.version}.tar.gz") / "pickaxe-hiveos"
        manifest = folder / "h-manifest.conf"
        manifest.write_text(
            manifest.read_text()
            .replace("/hive/miners/custom/pickaxe-hiveos", folder.as_posix())
            .replace("/var/log/miner/pickaxe-hiveos", (folder / "log").as_posix())
        )
        argv_file = folder / "argv"
        env = dict(os.environ, PICKAXE_ARGV_FILE=str(argv_file),
                   CUSTOM_URL="stratum2+tcp://h:3340/K", CUSTOM_TEMPLATE="addr.rig",
                   CUSTOM_PASS="", CUSTOM_USER_CONFIG="--chipnet\n--intensity 90")
        subprocess.run([BASH, "-c", ". ./h-manifest.conf; . ./h-config.sh"], cwd=folder,
                       env=env, check=True)
        args_file = folder / "pickaxe.args"
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE(args_file.stat().st_mode) & 0o077, 0)
        subprocess.run([BASH, (folder / "h-run.sh").as_posix()], env=env, check=True,
                       stdout=subprocess.DEVNULL)
        argv = recorded(argv_file)
        self.assertTrue(argv[3].endswith("/pickaxe.json"), argv)
        self.assertEqual(argv[:3] + argv[4:], [
            "farm-os", "mine", "--config",
            "--pool", "stratum2+tcp://h:3340/K", "--user", "addr.rig", "--password", "",
            "--", "--chipnet", "--intensity", "90",
        ])
        self.assertTrue((folder / "log" / "pickaxe.log").is_file())
        shown = subprocess.run(
            [BASH, "-c", f'. "{folder.as_posix()}/h-stats.sh"; printf "%s\\n%s" "$khs" "$stats"'],
            env=env, check=True, capture_output=True, text=True,
        ).stdout.splitlines()
        self.assertEqual(shown[0], "1.500")
        self.assertEqual(json.loads(shown[1])["hs"], [1500])

    def test_mmpos_passes_its_arguments_unchanged(self):
        folder = self.unpack(f"pickaxe-mmpos-{self.version}.tar.gz") / "pickaxe-mmpos"
        argv_file = folder / "argv"
        env = dict(os.environ, PICKAXE_ARGV_FILE=str(argv_file))
        given = ["--coin", "PHOTON", "--pool", "solo:1", "--user", "a.rig",
                 "--password", "two words", "--api-port", "4000", "--intensity", "90"]
        subprocess.run([BASH, (folder / "mmp-launch.sh").as_posix(), *given], env=env,
                       check=True)
        argv = recorded(argv_file)
        self.assertTrue(argv[3].endswith("/pickaxe.json"), argv)
        self.assertEqual(argv[:3] + argv[4:], ["farm-os", "mine", "--config", *given])
        line = subprocess.run([BASH, (folder / "mmp-stats.sh").as_posix(), "1", "log"], env=env,
                              check=True, capture_output=True, text=True).stdout
        self.assertEqual(json.loads(line)["hash"], [1500])

    @unittest.skipIf(os.name == "nt", "the stub miner is a shell script")
    def test_raveos_start_and_stats_with_a_fake_ravinos(self):
        rave = self.unpack(f"pickaxe-raveos-{self.version}.zip")
        fake = Path(tempfile.mkdtemp(dir=self.tmp))
        record = fake / "record.json"
        (fake / "ravinos.py").write_text(f"""
import json
RECORD = {str(record)!r}
def get_config():
    return {{"miner_dir": {str(rave)!r}, "args": "--chipnet --intensity 90",
            "auth_config": {{"ewal": "wallet"}},
            "coins": [{{"pools": [{{"url": "stratum2+tcp://h:3340/K", "user": "addr.rig",
                                   "password": ""}}]}}]}}
def get_stats():
    return {{"mpu": [{{"pci_id": 1, "temp": 0}}, {{"pci_id": 2, "temp": 50}}]}}
def run(command):
    json.dump({{"run": command}}, open(RECORD, "w"))
def set_stats(stats):
    json.dump({{"stats": stats}}, open(RECORD, "w"))
""")
        env = dict(os.environ, PYTHONPATH=str(fake))
        subprocess.run([sys.executable, str(rave / "RAVINOS" / "start.py")], env=env,
                       check=True)
        self.assertEqual(json.loads(record.read_text())["run"], " ".join([
            str(rave / "pickaxe"), "farm-os", "mine", "--config", str(rave / "pickaxe.json"),
            "--pool", "stratum2+tcp://h:3340/K", "--user", "addr.rig",
            "--", "--chipnet", "--intensity", "90",
        ]))
        subprocess.run([sys.executable, str(rave / "RAVINOS" / "stats.py")], env=env,
                       check=True)
        stats = json.loads(record.read_text())["stats"]
        self.assertEqual(stats["mpu"][0]["hash_rate1"], 1500.0)
        self.assertEqual(stats["mpu"][0]["temp"], 61)
        self.assertEqual(stats["mpu"][0]["shares"], {"accepted": 3, "invalid": 0, "rejected": 1})
        self.assertNotIn("hash_rate1", stats["mpu"][1])
        self.assertEqual(stats["shares"], {"accepted": 3, "invalid": 0, "rejected": 1})


class RaveosScripts(unittest.TestCase):
    """start.py's command line and stats.py's mapping, on every system."""

    def load(self, name):
        # No __pycache__ inside the package folder.
        sys.dont_write_bytecode = True
        sys.modules["ravinos"] = types.ModuleType("ravinos")
        path = ROOT / "packaging" / "farm-os" / "raveos" / "RAVINOS" / f"{name}.py"
        spec = importlib.util.spec_from_file_location(f"raveos_{name}", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_command_line_and_stats_mapping(self):
        start = self.load("start")
        cfg = {"miner_dir": "/m", "args": ["--chipnet"], "auth_config": {"ewal": "wallet"},
               "coins": [{"pools": [{"url": "h:3340", "user": "", "password": "KEY"}]}]}
        self.assertEqual(start.command_line(cfg).split(" "), [
            os.path.join("/m", "pickaxe"), "farm-os", "mine", "--config",
            os.path.join("/m", "pickaxe.json"), "--pool", "h:3340", "--password", "KEY",
            "--user", "wallet", "--", "--chipnet",
        ])
        cfg["args"] = "--intensity 90 --device '0 1'"
        with self.assertRaises(ValueError):
            start.command_line(cfg)
        stats = self.load("stats").apply(
            {"mpu": [{"pci_id": 3, "temp": 50}, {"pci_id": 4, "temp": 52}]},
            {"gpus": [{"pci_bus": 3, "hash_rate": 2.5e6, "temp": 0, "accepted": 2,
                       "rejected": 0}], "accepted": 2, "rejected": 0},
        )
        self.assertEqual(stats["mpu"][0]["hash_rate1"], 2.5e6)
        self.assertEqual(stats["mpu"][0]["temp"], 50)
        self.assertNotIn("hash_rate1", stats["mpu"][1])
        self.assertEqual(stats["shares"], {"accepted": 2, "invalid": 0, "rejected": 0})


if __name__ == "__main__":
    unittest.main()
