#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
#
# User-mode client for the NanoChronometer Windows driver.
# Reads the key=value hypervisor report via DeviceIoControl.
#
# Usage (from an admin prompt on the target Windows machine):
#   python query.py                 # query \\.\NanoChronometer
#   python query.py --wait          # retry while the driver comes up

import argparse
import ctypes
import sys
import time

CTL_CODE = (0x22 << 16) | (0x801 << 2) | 0  # 0x222004, METHOD_BUFFERED

GENERIC_NONE = 0x00000000
OPEN_EXISTING = 3
INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value


def query(device: str, tries: int = 1, delay: float = 0.0) -> None:
    for attempt in range(tries):
        h = ctypes.windll.kernel32.CreateFileW(
            device,
            GENERIC_NONE,
            0,
            None,
            OPEN_EXISTING,
            0,
            None,
        )
        if h != INVALID_HANDLE_VALUE:
            break
        if attempt + 1 < tries:
            time.sleep(delay)
    else:
        sys.exit(f"error: cannot open {device} — is the driver loaded?")

    try:
        out = ctypes.create_string_buffer(2048)
        returned = ctypes.c_ulong(0)
        ok = ctypes.windll.kernel32.DeviceIoControl(
            h,
            CTL_CODE,
            None,
            0,
            out,
            len(out),
            ctypes.byref(returned),
            None,
        )
        if not ok:
            sys.exit(f"error: DeviceIoControl failed (GetLastError "
                     f"{ctypes.get_last_error() & 0xFFFF})")
        print(out.raw[: returned.value].decode("utf-8", "replace"))
    finally:
        ctypes.windll.kernel32.CloseHandle(h)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--device", default=r"\\.\NanoChronometer")
    ap.add_argument("--wait", action="store_true", help="poll until available")
    args = ap.parse_args()

    tries = 60 if args.wait else 1
    query(args.device, tries=tries, delay=0.5)


if __name__ == "__main__":
    main()