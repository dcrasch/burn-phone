//! SqueezeNet as one fused QNN graph.
//!
//! The op list mirrors the generated `Model::forward`: every conv, ReLU, pool,
//! concat and the final average pool goes into a single graph, so intermediate
//! tensors stay on the Hexagon NPU and only the input and the 1000 logits cross
//! to the host.

use std::collections::HashMap;
use std::sync::OnceLock;

use burn_npu::NpuBurnBackend as Backend;
use burn_npu::backends::qualcomm::{FusedGraph, FusedOp, FusedTensor, OpParam};
use burn_store::ModuleSnapshot;

use crate::classifier::generated;

/// Graph built once from the model's weights, then reused for every frame.
/// `None` means the graph could not be built and the per-op path is used.
static GRAPH: OnceLock<Option<&'static FusedGraph>> = OnceLock::new();

/// Runs `input` (NCHW, 1×3×224×224) through SqueezeNet as one NPU graph and
/// returns the 1000 logits. The fused graph is used by default; set the
/// `debug.burnnpu.fused` system property to `0` to force the slower per-op
/// path. `None` means the caller should use the per-op path.
pub fn run(input: &[f32]) -> Option<Vec<f32>> {
    if fused_disabled() {
        return None;
    }
    if !burn_npu::backends::qualcomm::npu_enabled() {
        log::info!("fused: NPU disabled by force-CPU mode");
        return None;
    }
    let graph: &'static FusedGraph =
        match GRAPH.get_or_init(|| build().map(|g| &*Box::leak(Box::new(g)))) {
            Some(graph) => graph,
            None => {
                log::warn!("fused: could not describe the SqueezeNet graph");
                return None;
            }
        };
    let nhwc = nchw_to_nhwc(input, 3, 224, 224);
    log::info!("fused: executing one graph with {} ops", graph.ops.len());
    let result = burn_npu::backends::qualcomm::run_fused("squeezenet", graph, &nhwc);
    if result.is_none() {
        log::warn!("fused: graph execution failed; falling back to per-op path");
    }
    result
}

/// Whether the fused graph has been explicitly disabled via the Android system
/// property. The fused path is the default because it is much faster than one
/// graph per op.
fn fused_disabled() -> bool {
    static DISABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        #[cfg(target_os = "android")]
        {
            std::process::Command::new("getprop")
                .arg("debug.burnnpu.fused")
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
                .unwrap_or(false)
        }
        #[cfg(not(target_os = "android"))]
        {
            false
        }
    });
    *DISABLED
}

/// Reads the weights from the embedded model and describes the whole network.
fn build() -> Option<FusedGraph> {
    let device = Default::default();
    let model = generated::Model::<Backend>::from_embedded(&device);

    let mut weights: HashMap<String, (Vec<usize>, Vec<f32>)> = HashMap::new();
    for snapshot in model.collect(None, None, true) {
        let name = snapshot.full_path();
        let dims: Vec<usize> = snapshot.shape.iter().copied().collect();
        let data = snapshot
            .to_data()
            .ok()?
            .convert::<f32>()
            .into_vec::<f32>()
            .ok()?;
        weights.insert(name, (dims, data));
    }

    let mut g = Graph::default();
    let input = g.tensor(vec![1, 224, 224, 3], None);
    let conv = |g: &mut Graph,
                index: usize,
                x: usize,
                kernel: usize,
                stride: usize,
                pad: usize|
     -> Option<usize> {
        let (wdims, wdata) = weights.get(&format!("conv2d{index}.weight"))?;
        let (bdims, bdata) = weights.get(&format!("conv2d{index}.bias"))?;
        let (o, i, kh, kw) = (wdims[0], wdims[1], wdims[2], wdims[3]);
        if kh != kernel || kw != kernel || bdims[0] != o {
            return None;
        }
        let hwio = oihw_to_hwio(wdata, o, i, kh, kw);
        let w = g.tensor(dims(&[kh, kw, i, o]), Some(hwio));
        let b = g.tensor(dims(&[o]), Some(bdata.clone()));
        let xd = g.shape(x).to_vec();
        let oh = (xd[1] as usize + 2 * pad - kernel) / stride + 1;
        let ow = (xd[2] as usize + 2 * pad - kernel) / stride + 1;
        let out = g.tensor(dims(&[1, oh, ow, o]), None);
        g.op(
            "Conv2d",
            vec![x, w, b],
            out,
            vec![
                OpParam::U32Tensor("stride", vec![2], vec![stride as u32, stride as u32]),
                OpParam::U32Tensor(
                    "pad_amount",
                    vec![2, 2],
                    vec![pad as u32, pad as u32, pad as u32, pad as u32],
                ),
                OpParam::U32Tensor("dilation", vec![2], vec![1, 1]),
                OpParam::U32("group", 1),
            ],
        );
        Some(out)
    };

    let relu = |g: &mut Graph, x: usize| {
        let out = g.tensor(g.shape(x).to_vec(), None);
        g.op("Relu", vec![x], out, vec![]);
        out
    };
    let max_pool = |g: &mut Graph, x: usize| {
        let xd = g.shape(x).to_vec();
        let oh = (xd[1] as usize - 3) / 2 + 1;
        let ow = (xd[2] as usize - 3) / 2 + 1;
        let out = g.tensor(dims(&[1, oh, ow, xd[3] as usize]), None);
        g.op(
            "PoolMax2d",
            vec![x],
            out,
            vec![
                OpParam::U32Tensor("filter_size", vec![2], vec![3, 3]),
                OpParam::U32Tensor("stride", vec![2], vec![2, 2]),
                OpParam::U32Tensor("pad_amount", vec![2, 2], vec![0, 0, 0, 0]),
                OpParam::U32("rounding_mode", 0),
            ],
        );
        out
    };
    let concat = |g: &mut Graph, a: usize, b: usize| {
        let ad = g.shape(a).to_vec();
        let bd = g.shape(b).to_vec();
        let out = g.tensor(
            dims(&[1, ad[1] as usize, ad[2] as usize, (ad[3] + bd[3]) as usize]),
            None,
        );
        g.op("Concat", vec![a, b], out, vec![OpParam::U32("axis", 3)]);
        out
    };
    // One fire block: squeeze 1x1, then 1x1 and 3x3 expands concatenated.
    let fire = |g: &mut Graph, x: usize, squeeze: usize, e1: usize, e3: usize| -> Option<usize> {
        let s = conv(g, squeeze, x, 1, 1, 0)?;
        let s = relu(g, s);
        let a = conv(g, e1, s, 1, 1, 0)?;
        let a = relu(g, a);
        let b = conv(g, e3, s, 3, 1, 1)?;
        let b = relu(g, b);
        Some(concat(g, a, b))
    };

    // Same order as the generated `Model::forward`.
    let x = conv(&mut g, 1, input, 3, 2, 0)?;
    let x = relu(&mut g, x);
    let x = max_pool(&mut g, x);
    let x = fire(&mut g, x, 2, 3, 4)?;
    let x = fire(&mut g, x, 5, 6, 7)?;
    let x = max_pool(&mut g, x);
    let x = fire(&mut g, x, 8, 9, 10)?;
    let x = fire(&mut g, x, 11, 12, 13)?;
    let x = max_pool(&mut g, x);
    let x = fire(&mut g, x, 14, 15, 16)?;
    let x = fire(&mut g, x, 17, 18, 19)?;
    let x = fire(&mut g, x, 20, 21, 22)?;
    let x = fire(&mut g, x, 23, 24, 25)?;

    let x = conv(&mut g, 26, x, 1, 1, 0)?;
    let x = relu(&mut g, x);
    let out = g.tensor(dims(&[1, 1, 1, 1000]), None);
    g.op(
        "PoolAvg2d",
        vec![x],
        out,
        vec![
            OpParam::U32Tensor("filter_size", vec![2], vec![13, 13]),
            OpParam::U32Tensor("stride", vec![2], vec![13, 13]),
            OpParam::U32Tensor("pad_amount", vec![2, 2], vec![0, 0, 0, 0]),
            OpParam::U32("rounding_mode", 0),
            OpParam::Bool("count_pad_for_edges", false),
        ],
    );

    Some(g.finish(input, out))
}

fn dims(d: &[usize]) -> Vec<u32> {
    d.iter().map(|&v| v as u32).collect()
}

/// Accumulates tensors and ops for a [`FusedGraph`].
#[derive(Default)]
struct Graph {
    tensors: Vec<FusedTensor>,
    ops: Vec<FusedOp>,
}

impl Graph {
    fn tensor(&mut self, dims: Vec<u32>, data: Option<Vec<f32>>) -> usize {
        self.tensors.push(FusedTensor { dims, data });
        self.tensors.len() - 1
    }

    fn shape(&self, index: usize) -> &[u32] {
        &self.tensors[index].dims
    }

    fn op(
        &mut self,
        op_type: &'static str,
        inputs: Vec<usize>,
        output: usize,
        params: Vec<OpParam>,
    ) {
        self.ops.push(FusedOp {
            op_type,
            inputs,
            output,
            params,
        });
    }

    fn finish(self, input: usize, output: usize) -> FusedGraph {
        FusedGraph {
            tensors: self.tensors,
            ops: self.ops,
            input,
            output,
        }
    }
}

/// Convert OIHW convolution weights to HWIO.
fn oihw_to_hwio(data: &[f32], o: usize, i: usize, kh: usize, kw: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; data.len()];
    for oc in 0..o {
        for ic in 0..i {
            for y in 0..kh {
                for x in 0..kw {
                    out[((y * kw + x) * i + ic) * o + oc] = data[((oc * i + ic) * kh + y) * kw + x];
                }
            }
        }
    }
    out
}

/// Convert an NCHW image to NHWC.
fn nchw_to_nhwc(data: &[f32], c: usize, h: usize, w: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; data.len()];
    for ci in 0..c {
        for y in 0..h {
            for x in 0..w {
                out[(y * w + x) * c + ci] = data[(ci * h + y) * w + x];
            }
        }
    }
    out
}
