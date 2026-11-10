---
name: Hexagon Benchmark
description: Build, deploy, and run the burn-phone CPU-vs-NPU benchmark on a physical Qualcomm Hexagon device, then read the results from logcat with `just benchmark`.
---

# Hexagon benchmark

Use this skill when you need to measure burn-phone on a real Qualcomm Hexagon
NPU, compare it with the CPU path, or verify that a QNN graph-build change still
executes on the NPU. The benchmark runs the embedded `tests/data/cat.jpg`
through SqueezeNet once on the CPU and once on the NPU, and logs the results so
no screenshot or UI interaction is needed.

## Prerequisites

- QAIRT / QNN SDK present. The default is
  `~/.cache/burn-phone/qairt/2.52.0.261104`; override with `QNN_SDK_ROOT`.
- A physical Snapdragon phone connected over USB with debugging authorized.
  Emulators always fall back to the CPU and are not useful here.
- `adb` available (the justfile finds it under `~/Android/Sdk/platform-tools`).
- If more than one device is attached, set `ANDROID_SERIAL=<serial>`. With one
  phone the recipes pick it automatically.

Check the target first:

```sh
just devices
adb -s <serial> shell getprop ro.soc.model
```

## Run the benchmark

```sh
just benchmark
```

This recipe builds the debug APK, installs it, clears logcat, sets the
`debug.burnnpu.benchmark` system property to `1`, force-stops and relaunches the
app, waits for the result, prints the `BENCH` lines, and resets the property to
`0` on exit. The app detects the property at startup and runs the benchmark
automatically, so there is no button to tap.

To read the result already in the log buffer without rebuilding or relaunching:

```sh
just benchmark-log
```

## Expected output

```text
BENCH requested at startup
BENCH cpu total_ms=154.0 npu_ops=0 top=Egyptian cat:63.4%,lynx:12.3%,...
BENCH npu build_ms=223.1 compute_ms=1.0 total_ms=2.6 npu_ops=64 top=Egyptian cat:63.3%,lynx:12.5%,...
BENCH summary total_speedup=59.11 top_match=true
BENCH done
```

Field meanings:

| Field | Meaning |
| --- | --- |
| `cpu total_ms` | Full CPU classification: preprocess + forward pass. `npu_ops=0`. |
| `npu build_ms` | One-time QNN graph construction, measured during the untimed warm-up run. |
| `npu compute_ms` | Time spent inside `graphExecute` on the timed NPU run. |
| `npu total_ms` | Wall time of the timed NPU run. It includes preprocessing and per-op host overhead but not `build_ms`. |
| `npu npu_ops` | Ops that actually executed on the Hexagon NPU. `0` means the CPU fallback ran everything. |
| `top` | Top 4 classes with probabilities. |
| `total_speedup` | CPU total divided by NPU total. Values below 1 mean the CPU path is faster end to end. |
| `top_match` | Whether CPU and NPU chose the same top classes. |

On SM8475 with SDK 2.52, the default fused path reports 64 NPU ops, a one-time
graph build of roughly 0.2–10 s, and a total of about 2.6–4.8 ms. That is roughly
30–60× faster than the CPU (~144–154 ms), with the same top-1 class. Preprocessing
is ~1.5–2 ms and the NPU model execution is ~1 ms; the build is excluded from the
timed total. The per-op fallback is much slower: 63 separate graphs and host
dispatch overhead that makes the total similar to the CPU.

## Reading timing correctly

- `build_ms` is one-time work. It is excluded from the timed NPU run, so do not
  add it to `compute_ms` or `total_ms`.
- `compute_ms` is the NPU execution only. Use it to judge the NPU itself.
- `total_ms` is the end-to-end number to compare against the CPU total.
- The fused graph is the default. To force the old per-op path for comparison,
  run `adb shell setprop debug.burnnpu.fused 0` before launching.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `No physical phone found.` | Connect a phone, authorize USB debugging, or set `ANDROID_SERIAL`. |
| `Timed out waiting for the benchmark` | Check `/tmp/burn-phone-benchmark.log`; the app may have crashed or failed to install. Also confirm the app is still `dev.burnphone.app`. |
| `npu_ops=0` on a Snapdragon phone | The NPU build failed and silently fell back to the CPU. Search logcat for `QnnDsp`, `Failed to create InputDef`, or `Graph name is a duplicate`; see `docs/qnn-graph-build-pitfalls.md`. |
| Benchmark reruns on every launch | The script was interrupted before resetting the property. Run `adb shell setprop debug.burnnpu.benchmark 0`. |
| `just benchmark` rebuilds slowly | The QNN APK is large. Set `QNN_HEXAGON_VERSIONS="69"` for a V69 device to shrink it. |

FastRPC and SELinux warnings (`avc: denied` for `vendor_xdsp_device`,
`remote_session_control failed`) are normal on production phones and do not mean
the NPU failed.

## Related documentation

- `docs/burn-npu-qnn-setup.md` — SDK setup, packaging, per-op timing, and the
  benchmark section.
- `docs/qnn-graph-build-pitfalls.md` — duplicate graph/name ids and tensor
  parameter registration failures.
- `justfile` — `benchmark` and `benchmark-log` recipes.
