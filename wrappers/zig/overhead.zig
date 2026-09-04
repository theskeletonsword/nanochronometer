const std = @import("std");
const ncmod = @import("nanochronometer.zig");

pub fn main() !void {
    var nc = try ncmod.NanoChronometer.init();
    defer nc.deinit();
    _ = nc.calibrate(50);
    std.debug.print("native overhead cycles: {d}\n", .{nc.nativeOverheadCycles()});
    std.debug.print("Zig -> native cycles: {d}\n", .{nc.ffiOverheadCycles(10000)});
}
