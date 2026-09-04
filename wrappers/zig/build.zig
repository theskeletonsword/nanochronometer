const std = @import("std");
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const exe = b.addExecutable(.{ .name = "nanochrono_overhead", .root_source_file = b.path("overhead.zig"), .target = target, .optimize = optimize });
    exe.addIncludePath(b.path("../.."));
    exe.linkSystemLibrary("nanochrono");
    b.installArtifact(exe);
}
