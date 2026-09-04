from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "python"))
from nanochronometer.core import NanoChronometer

with NanoChronometer() as nc:
    snap = nc.nanoclock_snapshot()
    print("time_utc", nc.format_unix_time_ns(snap.unix_time_ns))
    print("route", nc.clock_route_name(snap.route), "simd", nc.simd_family_name(snap.best_simd_family))
    print("native_wrapper_baseline_cycles", nc.native_wrapper_baseline_cycles())
    print("python_ctypes_call_overhead_cycles", nc.ctypes_call_overhead_cycles())
