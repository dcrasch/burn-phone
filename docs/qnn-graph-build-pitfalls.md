# QNN graph-build pitfalls: duplicate names, IDs and tensor params

The vendored backend builds QNN graphs by hand instead of using the SDK's
`QnnModel` helper. Two classes of bugs showed up only when the graph reached the
Hexagon DSP. Both fail during graph construction or `graphFinalize`, not during
execution, so they look like a generic "NPU unavailable" fallback unless you
read the `QnnDsp` lines in logcat.

These notes apply to the code in
`vendor/burn-npu/src/backends/qualcomm/qnn.rs` and were verified with QAIRT /
QNN SDK `2.52.0.261104` on a Snapdragon SM8475 (Hexagon V69).

---

## 1. Duplicate graph and node names (`Graph name is a duplicate`)

### Symptom

```text
QnnDsp <E> Graph name is a duplicate: matmul_512x512x512
QnnDsp <E> Failed to create graph
```

or, for the fused network:

```text
QnnDsp <E> Graph name is a duplicate: squeezenet
```

The backend then falls back to the CPU, so the model still produces output and
the failure is easy to miss.

### Root cause

A QNN context keeps a registry of graph names. Every name passed to
`graphCreate(context, name, ...)` must be unique for the lifetime of that
context. We originally derived names from the op and shape only:

```text
matmul_512x512x512
Conv2d
PoolMax2d
```

That is fine for the first build, but not for a retry. A graph that fails
partway through construction is not inserted into the graph cache, so the next
attempt built it again under the same name. Using the cache length as a suffix
did not help for the same reason: a failed build does not increase the length.

The fused graph had the same problem for a different reason: every node was
named after its op type, so two `Conv2d` nodes collided even within one graph.

### Fix

Allocate a process-wide, monotonically increasing build id **before** attempting
the build, and put it in every graph and node name:

```rust
static OP_GRAPH_BUILD_ID: AtomicUsize = AtomicUsize::new(0);
static FUSED_GRAPH_BUILD_ID: AtomicUsize = AtomicUsize::new(0);

let build_id = OP_GRAPH_BUILD_ID.fetch_add(1, Ordering::Relaxed);
let graph_name = format!("matmul_{m}x{k}x{n}_{build_id}");
let node_name = format!("matmul_{build_id}");
```

```rust
let build_id = FUSED_GRAPH_BUILD_ID.fetch_add(1, Ordering::Relaxed);
let graph_name = format!("{key}_{build_id}");
let node_name = format!("{key}_{op_type}_{index}_{build_id}");
```

Rules of thumb:

- Never derive uniqueness from cache size, a successful-build count, or the
  shape alone.
- Allocate the id before the first fallible call, so a retry after a failed
  build gets a new name too.
- Treat graph names as global to the context and node names as unique within a
  graph.

### Related: tensor IDs are per graph

`tensorCreateGraphTensor` assigns each tensor an `id`. The tensors attached to a
node must carry the ids returned by that call; we copy them out immediately:

```rust
ids[i] = tensor_v1_mut(&mut sys_t).as_ref().unwrap().id;
// later, for every node input/output:
tensor_v1_mut(&mut sys_t).as_mut().unwrap().id = ids[input_index];
```

Building a fresh `Qnn_Tensor_t` from shape information at execute time leaves the
id at zero and gives:

```text
QnnDsp <E> Expected Tensor ID: 42 not found in user-provided tensors
```

Graph **names** must be unique per context; tensor **ids** must match the ones
created in that graph. Do not confuse the two.

---

## 2. Tensor params (`Failed to create InputDef ... param stride`)

### Symptom

```text
QnnDsp <E> Failed to create InputDef for Op Conv2d_0 (qti.aisw::Conv2d) param stride
QnnDsp <E> Failed to set the params for the op: Conv2d
QnnDsp <E> Failed to create the op Conv2d_0
```

The same error appears for `PoolMax2d` on `filter_size`, and for `pad_amount`
and `dilation` on convolutions. Scalar params such as `group` or `axis` are
fine; only tensor params fail.

### Background

QNN distinguishes scalar params (`QNN_PARAMTYPE_SCALAR`) from tensor params
(`QNN_PARAMTYPE_TENSOR`). Convolution strides, padding, dilation and pooling
kernel sizes are tensor params: static `UINT_32` tensors described with a
`Qnn_Tensor_t` embedded in `Qnn_Param_t`. `OpParam::U32Tensor` exists for this.

### Root causes

1. **The tensor param was never registered with the graph.** The DSP backend
   resolves every tensor param as a graph tensor. A fully populated
   `Qnn_Param_t` is not enough; call `tensorCreateGraphTensor(graph, &mut tensor)`
   before `graphAddNode`. The SDK's `QnnModel::addNode` does this indirectly
   through `addTensor`. Without it the DSP cannot produce an `InputDef` for the
   param and rejects the whole op.

2. **Quantization fields were left zeroed.** A zero-initialised
   `Qnn_QuantizeParams_t` does not mean "unquantized": enum value zero is
   `QNN_DEFINITION_IMPL_GENERATED`. The DSP then treats the `UINT_32` param as a
   generated/quantized tensor and rejects it. Set both fields explicitly, as
   `qnn_model_float.cpp` does:

   ```rust
   v1.quantizeParams.encodingDefinition = QNN_DEFINITION_UNDEFINED;
   v1.quantizeParams.quantizationEncoding = QNN_QUANTIZATION_ENCODING_UNDEFINED;
   ```

3. **Backing memory was freed or moved too early.** QNN reads each op's
   definition, parameter names, tensor dimensions and values until
   `graphFinalize` returns. It is not enough to keep the `Vec<Qnn_Param_t>`
   alive: the `CString` names, dimension arrays and value arrays must stay at a
   stable address too. Keep them in per-op storage (`OpStorage`) for the whole
   build, never in a temporary that is dropped after `graphAddNode`.

4. **Tensor param names collided in the fused graph.** Tensor params are
   registered graph tensors, so their tensor names must be unique within the
   graph. The op parameter key must still be the bare name (`stride`) so QNN can
   match it to the op definition, but the registered tensor name is namespaced
   per node:

   ```rust
   sp.name = "stride";                                  // op parameter key
   v1.name = format!("{node_name}_stride");             // graph tensor name
   ```

### Correct tensor-param setup

```rust
let mut sp: sys::Qnn_Param_t = std::mem::zeroed();
sp.name = param_name.as_ptr();          // bare op parameter name
sp.paramType = QNN_PARAMTYPE_TENSOR;

let t = &mut sp.__bindgen_anon_1.tensorParam;
t.version = QNN_TENSOR_VERSION_1;
let v1 = tensor_v1_mut(t);
(*v1).name = tensor_name.as_ptr();      // unique per node
(*v1).type_ = QNN_TENSOR_TYPE_STATIC;
(*v1).dataFormat = QNN_TENSOR_DATA_FORMAT_FLAT_BUFFER;
(*v1).dataType = QNN_DATATYPE_UINT_32;
(*v1).quantizeParams.encodingDefinition = QNN_DEFINITION_UNDEFINED;
(*v1).quantizeParams.quantizationEncoding = QNN_QUANTIZATION_ENCODING_UNDEFINED;
(*v1).rank = dims.len() as u32;
(*v1).dimensions = dims.as_mut_ptr();
(*v1).memType = QNN_TENSORMEMTYPE_RAW;
(*v1).__bindgen_anon_1.clientBuf.data = vals.as_mut_ptr().cast();
(*v1).__bindgen_anon_1.clientBuf.dataSize = (vals.len() * 4) as u32;

// Required: register it as a graph tensor before adding the node.
tensorCreateGraphTensor(graph, t);
```

### Required order per node

1. Describe and register the op's ordinary input, weight, bias and output
   tensors with `tensorCreateGraphTensor`.
2. Copy their returned `id`s into the node's `inputTensors` / `outputTensors`.
3. Build scalar and tensor params.
4. Register **each tensor param** with `tensorCreateGraphTensor`.
5. Call `graphAddNode`.
6. After every node is added, call `graphFinalize`.

---

## Verification

After these fixes, logcat no longer contains `Graph name is a duplicate` or
`Failed to create InputDef`. On the SM8475 with SDK 2.52, the per-op SqueezeNet
path reports 63 NPU ops, a one-time graph build of roughly 1 s, and around
35 ms of NPU compute per classification (see `just benchmark`).

FastRPC / SELinux messages such as `avc: denied` for `vendor_xdsp_device` or
`adsprpc` `remote_session_control failed` are expected on a production phone and
do not indicate a graph-build failure.

## References

- SDK `share/QNN/converter/jni/QnnModel.cpp` — `QnnModel::addNode` and its
  `addTensor` helper, which calls `tensorCreateGraphTensor` for parameters.
- SDK `examples/QNN/converter/models/qnn_model_float.cpp` — the reference
  `UINT_32` tensor-param construction and quantization fields.
- `vendor/burn-npu/src/backends/qualcomm/qnn.rs` — `graph_for`,
  `op_graph_for`, `build_params` and `build_fused`.
- `vendor/burn-npu/src/backends/qualcomm/mod.rs` — `OpParam::U32Tensor`.
