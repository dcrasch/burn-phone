//! SqueezeNet 1.1 ImageNet classifier. The network runs as one fused graph on
//! the Hexagon NPU when available (see `squeezenet_fused`), with a per-op
//! burn-npu fallback and a CPU fallback. The model is generated from the ONNX
//! file at build time (see `build.rs`) and its weights are embedded in the
//! binary.

use burn::prelude::*;
use burn::tensor::activation::softmax;
use burn_npu::NpuBurnBackend as Backend;

const WIDTH: usize = 224;
const HEIGHT: usize = 224;
/// Input normalization used by the ONNX SqueezeNet, from the ONNX Model Zoo.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

pub(crate) mod generated {
    include!(concat!(env!("OUT_DIR"), "/model/squeezenet1_opset16.rs"));
}

include!(concat!(env!("OUT_DIR"), "/labels.rs"));

/// One ranked prediction.
#[derive(Clone, Debug)]
pub struct Prediction {
    pub label: &'static str,
    pub probability: f32,
}

/// The ranked predictions for one photo, and how many NPU ops ran for it.
#[derive(Clone, Debug)]
pub struct Classification {
    pub top: Vec<Prediction>,
    pub npu_ops: u64,
}

/// Classifies `rgba` (tightly packed, `width * height * 4` bytes) and returns
/// the `top` most likely ImageNet classes, highest probability first.
pub fn classify(rgba: &[u8], width: u32, height: u32, top: usize) -> Classification {
    let before = burn_npu::backends::qualcomm::npu_dispatch_count();
    let input = preprocess(rgba, width, height);
    let values = match crate::squeezenet_fused::run(&input) {
        Some(logits) => softmax_vec(&logits),
        None => forward_ops(&input),
    };

    let mut ranked: Vec<(usize, f32)> = values.into_iter().enumerate().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let top = ranked
        .into_iter()
        .take(top)
        .map(|(index, probability)| Prediction {
            label: LABELS[index],
            probability,
        })
        .collect();
    let npu_ops = burn_npu::backends::qualcomm::npu_dispatch_count() - before;
    Classification { top, npu_ops }
}

/// Runs the per-op model. Used when the fused NPU graph is unavailable. The
/// model is loaded once and kept, so its weight buffers stay at fixed
/// addresses for the process lifetime.
fn forward_ops(input: &[f32]) -> Vec<f32> {
    static MODEL: std::sync::OnceLock<generated::Model<Backend>> = std::sync::OnceLock::new();
    let model = MODEL.get_or_init(|| {
        let device = Default::default();
        generated::Model::<Backend>::from_embedded(&device)
    });
    let device = model.devices()[0].clone();
    let tensor = Tensor::<Backend, 1>::from_floats(input, &device).reshape([1, 3, HEIGHT, WIDTH]);
    softmax(model.forward(tensor), 1)
        .into_data()
        .convert::<f32>()
        .into_vec::<f32>()
        .expect("classifier output is f32")
}

/// Softmax over a flat logit vector.
fn softmax_vec(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    exps.into_iter().map(|e| e / sum).collect()
}

/// Resizes to 224×224 (stretching, like the reference demo) and applies the
/// ImageNet normalization, returning NCHW values.
///
/// This writes straight from the RGBA bytes into NCHW floats using an area
/// average for downscaling, which anti-aliases like the original Triangle
/// filter. Going through `image::imageops::resize` allocated two intermediate
/// buffers and made preprocessing (~140 ms) dominate the whole classification;
/// this version keeps the hot path allocation-free apart from the output.
fn preprocess(rgba: &[u8], width: u32, height: u32) -> Vec<f32> {
    let src_w = width as usize;
    let src_h = height as usize;
    let dst_w = WIDTH;
    let dst_h = HEIGHT;

    let mut chw = vec![0.0f32; 3 * dst_w * dst_h];
    for dy in 0..dst_h {
        let sy0 = dy * src_h / dst_h;
        let sy1 = (((dy + 1) * src_h).div_ceil(dst_h)).max(sy0 + 1).min(src_h);
        for dx in 0..dst_w {
            let sx0 = dx * src_w / dst_w;
            let sx1 = (((dx + 1) * src_w).div_ceil(dst_w)).max(sx0 + 1).min(src_w);

            let mut sum = [0u32; 3];
            for sy in sy0..sy1 {
                let row = sy * src_w;
                for sx in sx0..sx1 {
                    let pixel = (row + sx) * 4;
                    sum[0] += rgba[pixel] as u32;
                    sum[1] += rgba[pixel + 1] as u32;
                    sum[2] += rgba[pixel + 2] as u32;
                }
            }

            let count = ((sy1 - sy0) * (sx1 - sx0)) as f32 * 255.0;
            for c in 0..3 {
                let value = sum[c] as f32 / count;
                chw[c * dst_w * dst_h + dy * dst_w + dx] = (value - MEAN[c]) / STD[c];
            }
        }
    }
    chw
}

#[cfg(test)]
mod tests {
    use super::classify;

    #[test]
    #[ignore = "slow: runs the full SqueezeNet on the CPU"]
    fn classifies_a_cat() {
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/cat.jpg"))
            .expect("sample image");
        let image = image::load_from_memory(&bytes).expect("decode").to_rgba8();
        let (w, h) = image.dimensions();
        let result = classify(&image.into_raw(), w, h, 4);
        let top = result.top;
        for p in &top {
            println!("{:>6.2}% {}", p.probability * 100.0, p.label);
        }
        assert_eq!(top.len(), 4);
        assert!(top[0].probability > top[1].probability);
    }
}
