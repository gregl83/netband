#!/usr/bin/env python3
"""Reject release binaries outside the supported GNU/Linux runtime baseline."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess

BASELINE = (2, 35)
TARGETS = {
    "x86_64-unknown-linux-gnu": ("Advanced Micro Devices X86-64", "/lib64/ld-linux-x86-64.so.2"),
    "aarch64-unknown-linux-gnu": ("AArch64", "/lib/ld-linux-aarch64.so.1"),
}


def audit(target, header, program, dynamic, versions):
    machine, interpreter = TARGETS[target]
    if not re.search(rf"Machine:\s+{re.escape(machine)}\s*$", header, re.MULTILINE):
        raise ValueError(f"Unexpected ELF architecture for {target}")
    if f"[Requesting program interpreter: {interpreter}]" not in program:
        raise ValueError(f"Unexpected ELF interpreter for {target}")
    libraries = sorted(set(re.findall(r"\(NEEDED\).*\[([^]]+)\]", dynamic)))
    allowed = {"libc.so.6", "libm.so.6", "libgcc_s.so.1", "libpthread.so.0",
               "libdl.so.2", "librt.so.1", Path(interpreter).name}
    if not libraries or set(libraries) - allowed:
        raise ValueError(f"Unexpected shared libraries: {libraries}")
    names = set(re.findall(r"\bGLIBC_[\w.]+", versions))
    if not names or any(not re.fullmatch(r"GLIBC_\d+(?:\.\d+)+", name) for name in names):
        raise ValueError(f"Missing or unsupported glibc symbol versions: {sorted(names)}")
    required = max(tuple(map(int, name.removeprefix("GLIBC_").split('.'))) for name in names)
    if required > BASELINE:
        raise ValueError(f"Requires glibc {required}, above supported baseline {BASELINE}")
    return {"target": target, "glibc_required": '.'.join(map(str, required)),
            "glibc_supported": '.'.join(map(str, BASELINE)),
            "interpreter": interpreter, "shared_libraries": libraries}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    outputs = [subprocess.check_output(["readelf", "--wide", flag, str(args.binary)],
                                      env=dict(os.environ, LC_ALL="C"), text=True)
               for flag in ("--file-header", "--program-headers", "--dynamic", "--version-info")]
    print(json.dumps(audit(args.target, *outputs), indent=2))
