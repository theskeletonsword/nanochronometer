'use strict';
const ffi = require('ffi-napi');
const ref = require('ref-napi');
const voidPtr = ref.refType(ref.types.void);
const libName = process.env.NANOCHRONO_LIB || 'nanochrono';
const lib = ffi.Library(libName, {
  nc_create: [voidPtr, []],
  nc_destroy: ['void', [voidPtr]],
  nc_calibrate: ['int', [voidPtr, 'uint']],
  nc_start: ['uint64', [voidPtr]],
  nc_stop: ['uint64', [voidPtr]],
  nc_now_cycles: ['uint64', [voidPtr]],
  nc_cycles_to_ns: ['uint64', [voidPtr, 'uint64']],
  nc_elapsed_ns: ['uint64', [voidPtr]],
  nc_measure_overhead_cycles: ['uint64', [voidPtr]],
  nc_measure_ffi_overhead_cycles: ['uint64', [voidPtr, 'uint']],
  nc_measure_wrapper_overhead_cycles: ['uint64', [voidPtr, 'int', 'uint']],
  nc_empty_call: ['uint64', []],
  nc_measure_callback_min_cycles: ['uint64', [voidPtr, 'pointer', voidPtr, 'uint']],
  nc_welch_t_score: ['double', ['pointer', 'uint', 'pointer', 'uint']],
  nc_instruction_family_available: ['int', ['uint']],
  nc_measure_aesenc_cycles: ['uint64', [voidPtr, 'uint', 'pointer']],
  nc_measure_sha256msg_cycles: ['uint64', [voidPtr, 'uint', 'pointer']],
  nc_measure_pclmul_cycles: ['uint64', [voidPtr, 'uint', 'pointer']],
  nc_crypto_backend_mask: ['int', []],
  nc_measure_kernel_timecall_overhead_cycles: ['uint64', [voidPtr, 'uint']],
  nc_measure_api_call_overhead_cycles: ['uint64', [voidPtr, 'uint']],
  nc_calibrate_clock_route: ['int', [voidPtr, 'int', 'uint', 'uint', 'uint', 'pointer']],
  nc_ntp_query: ['int', [voidPtr, 'string', 'uint', 'int', 'pointer']],
  nc_stable_clock_default_config: ['int', ['pointer']],
  nc_calibrate_cycles_per_ns: ['int', [voidPtr, 'pointer', 'pointer']],
  nc_cycles_to_ns_calibrated: ['uint64', ['uint64', 'double']],
  nc_raw_delta_to_ns_calibrated: ['uint64', ['uint64', 'uint64', 'double']],
  nc_clock_read_raw_route: ['uint64', [voidPtr, 'int']],
  nc_clock_stability_advice: ['string', []]
});

class NanoChronometer {
  constructor() {
    this.ctx = lib.nc_create();
    if (ref.isNull(this.ctx)) throw new Error('nc_create failed');
  }
  close() { if (this.ctx && !ref.isNull(this.ctx)) { lib.nc_destroy(this.ctx); this.ctx = ref.NULL; } }
  calibrate(ms = 50) { return lib.nc_calibrate(this.ctx, ms) !== 0; }
  start() { return BigInt(lib.nc_start(this.ctx)); }
  stop() { return BigInt(lib.nc_stop(this.ctx)); }
  nowCycles() { return BigInt(lib.nc_now_cycles(this.ctx)); }
  cyclesToNs(cycles) { return BigInt(lib.nc_cycles_to_ns(this.ctx, cycles.toString())); }
  elapsedNs() { return BigInt(lib.nc_elapsed_ns(this.ctx)); }
  nativeOverheadCycles() { return BigInt(lib.nc_measure_overhead_cycles(this.ctx)); }
  ffiOverheadCycles(iterations = 10000) { return BigInt(lib.nc_measure_ffi_overhead_cycles(this.ctx, iterations)); }
  nativeWrapperBaselineCycles(iterations = 10000) { return BigInt(lib.nc_measure_wrapper_overhead_cycles(this.ctx, 4, iterations)); }
  nodeFFIOverheadCycles(iterations = 10000) {
    let best = null;
    for (let i = 0; i < iterations; i++) {
      const a = BigInt(lib.nc_now_cycles(this.ctx));
      lib.nc_empty_call();
      const b = BigInt(lib.nc_now_cycles(this.ctx));
      const d = b >= a ? b - a : 0n;
      if (d > 0n && (best === null || d < best)) best = d;
    }
    return best || 0n;
  }
  callbackMinCycles(callback, iterations = 10000) {
    const cb = ffi.Callback('void', [voidPtr], callback);
    this._lastCallback = cb;
    return BigInt(lib.nc_measure_callback_min_cycles(this.ctx, cb, ref.NULL, iterations));
  }
  instructionAvailable(family) { return lib.nc_instruction_family_available(family) !== 0; }
  measureAES(blocks = 1024) { const buf = Buffer.alloc(64); return BigInt(lib.nc_measure_aesenc_cycles(this.ctx, blocks, buf)); }
  measureSHAInstr(blocks = 1024) { const buf = Buffer.alloc(64); return BigInt(lib.nc_measure_sha256msg_cycles(this.ctx, blocks, buf)); }
  measurePCLMUL(blocks = 1024) { const buf = Buffer.alloc(64); return BigInt(lib.nc_measure_pclmul_cycles(this.ctx, blocks, buf)); }
  cryptoBackendMask() { return lib.nc_crypto_backend_mask(); }
  kernelTimecallOverheadCycles(iterations = 1000) { return BigInt(lib.nc_measure_kernel_timecall_overhead_cycles(this.ctx, iterations)); }
  apiCallOverheadCycles(iterations = 1000) { return BigInt(lib.nc_measure_api_call_overhead_cycles(this.ctx, iterations)); }
  // For calibration/NTP, pass a Buffer with the native struct size from nanochrono.h.
  calibrateClockRoute(outBuffer, route = 0, samples = 200000, pinCpu = 0, cpuIndex = 0) { return lib.nc_calibrate_clock_route(this.ctx, route, samples, pinCpu, cpuIndex, outBuffer) !== 0; }
  ntpQuery(server = 'pool.ntp.org', outBuffer, timeoutMs = 1000, route = 0) { return lib.nc_ntp_query(this.ctx, server, timeoutMs, route, outBuffer) !== 0; }
  stableClockDefaultConfig(cfgBuffer) { return lib.nc_stable_clock_default_config(cfgBuffer) !== 0; }
  calibrateCyclesPerNs(cfgBuffer, stateBuffer) { return lib.nc_calibrate_cycles_per_ns(this.ctx, cfgBuffer, stateBuffer) !== 0; }
  cyclesToNsCalibrated(cycles, cyclesPerNs) { return BigInt(lib.nc_cycles_to_ns_calibrated(cycles.toString(), cyclesPerNs)); }
  rawDeltaToNsCalibrated(a, b, unitsPerNs) { return BigInt(lib.nc_raw_delta_to_ns_calibrated(a.toString(), b.toString(), unitsPerNs)); }
  readRawRoute(route = 0) { return BigInt(lib.nc_clock_read_raw_route(this.ctx, route)); }
  stabilityAdvice() { return lib.nc_clock_stability_advice(); }
}
module.exports = { NanoChronometer, lib };
