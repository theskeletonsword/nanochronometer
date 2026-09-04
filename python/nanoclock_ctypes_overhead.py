from nanochronometer.core import NanoChronometer

with NanoChronometer() as nc:
    snap = nc.nanoclock_snapshot()
    print("time_utc", nc.format_unix_time_ns(snap.unix_time_ns))
    print("route", nc.clock_route_name(snap.route), "simd", nc.simd_family_name(snap.best_simd_family))
    print("selected_raw", snap.selected_raw_units, "selected_ns", snap.selected_ns)
    print("native_wrapper_baseline_cycles", nc.native_wrapper_baseline_cycles())
    print("python_ctypes_call_overhead_cycles", nc.ctypes_call_overhead_cycles())
