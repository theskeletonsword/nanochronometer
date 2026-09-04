local ffi = require("ffi")
ffi.cdef[[
typedef struct nc_ctx nc_ctx_t;
typedef void (*nc_void_callback_t)(void *arg);
typedef struct nc_sample_stats {
  uint64_t count, min_cycles, max_cycles, mean_cycles, median_cycles, p90_cycles, p99_cycles;
  double variance_cycles, stdev_cycles;
} nc_sample_stats_t;
nc_ctx_t *nc_create(void);
void nc_destroy(nc_ctx_t *ctx);
int nc_calibrate(nc_ctx_t *ctx, uint32_t ms);
uint64_t nc_start(nc_ctx_t *ctx);
uint64_t nc_stop(nc_ctx_t *ctx);
uint64_t nc_now_cycles(nc_ctx_t *ctx);
uint64_t nc_cycles_to_ns(nc_ctx_t *ctx, uint64_t cycles);
uint64_t nc_elapsed_ns(nc_ctx_t *ctx);
uint64_t nc_measure_overhead_cycles(nc_ctx_t *ctx);
uint64_t nc_measure_ffi_overhead_cycles(nc_ctx_t *ctx, uint32_t iterations);
uint64_t nc_measure_wrapper_overhead_cycles(nc_ctx_t *ctx, int kind, uint32_t iterations);
uint64_t nc_empty_call(void);
uint64_t nc_measure_callback_min_cycles(nc_ctx_t *ctx, nc_void_callback_t cb, void *arg, uint32_t iterations);
uint64_t nc_measure_kernel_timecall_overhead_cycles(nc_ctx_t *ctx, uint32_t iterations);
uint64_t nc_measure_api_call_overhead_cycles(nc_ctx_t *ctx, uint32_t iterations);
int nc_analyze_samples(const uint64_t *samples, uint32_t count, nc_sample_stats_t *out_stats);
double nc_welch_t_score(const uint64_t *a, uint32_t na, const uint64_t *b, uint32_t nb);
]]
local lib = ffi.load(os.getenv("NANOCHRONO_LIB") or "nanochrono")
local M = {}
local Chrono = {}; Chrono.__index = Chrono
function M.new()
  local ctx = lib.nc_create()
  assert(ctx ~= nil, "nc_create failed")
  return setmetatable({ctx=ctx}, Chrono)
end
function Chrono:close() if self.ctx ~= nil then lib.nc_destroy(self.ctx); self.ctx=nil end end
function Chrono:calibrate(ms) return lib.nc_calibrate(self.ctx, ms or 50) ~= 0 end
function Chrono:start() return tonumber(lib.nc_start(self.ctx)) end
function Chrono:stop() return tonumber(lib.nc_stop(self.ctx)) end
function Chrono:now_cycles() return tonumber(lib.nc_now_cycles(self.ctx)) end
function Chrono:cycles_to_ns(cycles) return tonumber(lib.nc_cycles_to_ns(self.ctx, cycles)) end
function Chrono:elapsed_ns() return tonumber(lib.nc_elapsed_ns(self.ctx)) end
function Chrono:native_overhead_cycles() return tonumber(lib.nc_measure_overhead_cycles(self.ctx)) end
function Chrono:ffi_overhead_cycles(iters) return tonumber(lib.nc_measure_ffi_overhead_cycles(self.ctx, iters or 10000)) end
function Chrono:native_wrapper_baseline_cycles(iters) return tonumber(lib.nc_measure_wrapper_overhead_cycles(self.ctx, 5, iters or 10000)) end
function Chrono:kernel_timecall_overhead_cycles(iters) return tonumber(lib.nc_measure_kernel_timecall_overhead_cycles(self.ctx, iters or 1000)) end
function Chrono:api_call_overhead_cycles(iters) return tonumber(lib.nc_measure_api_call_overhead_cycles(self.ctx, iters or 1000)) end
function Chrono:lua_ffi_overhead_cycles(iters)
  iters = iters or 10000
  local best = nil
  for i=1,iters do
    local a = lib.nc_now_cycles(self.ctx)
    lib.nc_empty_call()
    local b = lib.nc_now_cycles(self.ctx)
    local d = tonumber(b - a)
    if d > 0 and (best == nil or d < best) then best = d end
  end
  return best or 0
end
function Chrono:callback_min_cycles(fn, iters)
  local cb = ffi.cast("nc_void_callback_t", fn)
  self._last_cb = cb
  return tonumber(lib.nc_measure_callback_min_cycles(self.ctx, cb, nil, iters or 10000))
end
return M
