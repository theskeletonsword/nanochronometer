const std = @import("std");
const c = @cImport({ @cInclude("nanochrono.h"); });

pub const SampleStats = c.nc_sample_stats_t;

pub const NanoChronometer = struct {
    ctx: *c.nc_ctx_t,

    pub fn init() !NanoChronometer {
        const p = c.nc_create();
        if (p == null) return error.CreateFailed;
        return .{ .ctx = p.? };
    }
    pub fn deinit(self: *NanoChronometer) void { c.nc_destroy(self.ctx); }
    pub fn calibrate(self: *NanoChronometer, ms: u32) bool { return c.nc_calibrate(self.ctx, ms) != 0; }
    pub fn start(self: *NanoChronometer) u64 { return c.nc_start(self.ctx); }
    pub fn stop(self: *NanoChronometer) u64 { return c.nc_stop(self.ctx); }
    pub fn nowCycles(self: *NanoChronometer) u64 { return c.nc_now_cycles(self.ctx); }
    pub fn cyclesToNs(self: *NanoChronometer, cycles: u64) u64 { return c.nc_cycles_to_ns(self.ctx, cycles); }
    pub fn elapsedNs(self: *NanoChronometer) u64 { return c.nc_elapsed_ns(self.ctx); }
    pub fn nativeOverheadCycles(self: *NanoChronometer) u64 { return c.nc_measure_overhead_cycles(self.ctx); }
    pub fn ffiOverheadCycles(self: *NanoChronometer, iterations: u32) u64 { return c.nc_measure_ffi_overhead_cycles(self.ctx, iterations); }
};

pub fn welchTScore(a: []const u64, b: []const u64) f64 {
    return c.nc_welch_t_score(a.ptr, @intCast(a.len), b.ptr, @intCast(b.len));
}
