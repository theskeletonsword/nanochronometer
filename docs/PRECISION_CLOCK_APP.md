# NanoChronometer Precision Clock App API

This build adds a clock-app style layer on top of NanoChronometer for GUI, CLI,
static libraries, dynamic libraries and language wrappers.

## Views

- `STOPWATCH`: elapsed-time chronometer view.
- `CLOCK`: wall-clock view with raw local time and optional NTP disciplined sample.
- `TIMER`: reserved timer view for the same UI switch.

The GUI can switch views with the `VIEW` button or `V`; it switches LOCAL/UTC
with the `ZONE` button or `Z`; it switches analog/digital with the existing
clock button or `C`; and it switches SIMPLE/NANO with the existing mode button.

## Raw local clock versus NTP clock

The raw clock path is always local and never performs network I/O.  It can use:

- x64: `RDTSC`, `LFENCE+RDTSC`, `MFENCE+LFENCE+RDTSC`, `RDTSCP+LFENCE`.
- ARM64: `CNTFRQ_EL0`, `CNTVCT_EL0`, `ISB+CNTVCT_EL0`, optional `PMCCNTR_EL0`.

The NTP path is explicit. It reports:

- local send/receive time,
- server transmit time,
- estimated offset,
- round-trip delay,
- socket setup overhead,
- send/receive overhead,
- kernel time-call overhead,
- API-call overhead,
- CPU migration detection.

## CLI examples

```bash
nanochrono_cli --clock --simple --local
nanochrono_cli --clock --nano --utc
nanochrono_cli --clock-once --nano --utc-offset -180 --ntp time.cloudflare.com
nanochrono_cli --calibrate-clock --pin-cpu 0
nanochrono_cli --ntp pool.ntp.org --nano --utc
```

## Exported C symbols

The public API is declared in `nanochrono.h` and exported by the static and
dynamic library builds:

- `nc_format_unix_time_ns_ex`
- `nc_precision_clock_default_config`
- `nc_clock_pin_thread_to_cpu`
- `nc_clock_current_cpu`
- `nc_measure_kernel_timecall_overhead_cycles`
- `nc_measure_api_call_overhead_cycles`
- `nc_calibrate_clock_route`
- `nc_ntp_query`

## Calibration and production notes

For best precision, run calibration on the target host, pin the measuring thread,
check for CPU migration, prefer invariant architectural counters, and validate the
platform policy for CPU frequency scaling and C-states. NanoChronometer reports
these measurements; it does not by itself certify regulatory compliance.

MiFID II / RTS 25 style deployments need operational controls outside the code:
traceable UTC synchronisation, monitoring, alerting, holdover policy, audit logs,
clock source redundancy, and documented maximum divergence procedures.
