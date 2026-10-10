# #### PR #42: RaveOS starts the miner with the command line this builds
# (raveos-foundation/custom_mining). Each pool becomes --pool and
# --password, the first pool's user (or the wallet) --user, and the extra
# arguments follow `--`; `pickaxe farm-os mine` maps them (src/farm_os.rs).
# Python only because RaveOS requires it.
import os
import shlex

import ravinos


def command_line(cfg):
    miner_dir = cfg.get("miner_dir") or os.getcwd()
    argv = [
        os.path.join(miner_dir, "pickaxe"),
        "farm-os",
        "mine",
        "--config",
        os.path.join(miner_dir, "pickaxe.json"),
    ]
    coins = cfg.get("coins") or [{}]
    pools = coins[0].get("pools") or []
    for pool in pools:
        if pool.get("url"):
            argv += ["--pool", pool["url"]]
        if pool.get("password"):
            argv += ["--password", pool["password"]]
    user = (pools[0].get("user") if pools else "") or (cfg.get("auth_config") or {}).get("ewal")
    if user:
        argv += ["--user", user]
    extra = cfg.get("args") or []
    if isinstance(extra, str):
        extra = shlex.split(extra)
    if extra:
        argv += ["--"] + list(extra)
    for value in argv:
        if not value or any(ch.isspace() for ch in value):
            raise ValueError("a pool, user or argument contains whitespace: {!r}".format(value))
    return " ".join(argv)


if __name__ == "__main__":
    ravinos.run(command_line(ravinos.get_config()))
