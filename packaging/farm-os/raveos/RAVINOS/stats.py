# #### PR #42: RaveOS reads the statistics this sets. The miner's status
# file gives each GPU's rate, temperature and shares, matched to RaveOS's
# GPUs by PCI bus; on any error the shares are zero and the system's own
# values stay. Python only because RaveOS requires it.
import json
import os
import subprocess

import ravinos


def miner_dir():
    try:
        return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    except NameError:
        return os.getcwd()


def apply(stats, report):
    by_bus = {gpu["pci_bus"]: gpu for gpu in report.get("gpus", [])}
    for mpu in stats.get("mpu", []):
        gpu = by_bus.get(mpu.get("pci_id"))
        if gpu is None:
            continue
        mpu["hash_rate1"] = float(gpu.get("hash_rate", 0))
        if gpu.get("temp"):
            mpu["temp"] = int(gpu["temp"])
        mpu["shares"] = {
            "accepted": int(gpu.get("accepted", 0)),
            "invalid": 0,
            "rejected": int(gpu.get("rejected", 0)),
        }
    stats["shares"] = {
        "accepted": int(report.get("accepted", 0)),
        "invalid": int(report.get("invalid", 0)),
        "rejected": int(report.get("rejected", 0)),
    }
    return stats


def main():
    stats = ravinos.get_stats()
    here = miner_dir()
    try:
        output = subprocess.run(
            [
                os.path.join(here, "pickaxe"),
                "farm-os",
                "stats",
                "--os",
                "raveos",
                "--config",
                os.path.join(here, "pickaxe.json"),
            ],
            capture_output=True,
            timeout=5,
            check=True,
        ).stdout
        stats = apply(stats, json.loads(output))
    except Exception:  # noqa: BLE001 - any failure reports zero shares
        stats["shares"] = {"accepted": 0, "invalid": 0, "rejected": 0}
    ravinos.set_stats(stats)


if __name__ == "__main__":
    main()
