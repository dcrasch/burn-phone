//! Generates the SqueezeNet classifier from its ONNX file and the ImageNet
//! labels as a Rust table. Weights are embedded in the binary.

use std::{
    env,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
};

use burn_onnx::{LoadStrategy, ModelGen};

const MODEL_FILE: &str = "src/model/squeezenet1_opset16.onnx";
const LABEL_SOURCE_FILE: &str = "src/model/label.txt";

fn main() {
    println!("cargo:rerun-if-changed=src/model");
    println!("cargo:rerun-if-changed=build.rs");

    ModelGen::new()
        .input(MODEL_FILE)
        .out_dir("model/")
        .load_strategy(LoadStrategy::Embedded)
        .run_from_script();

    generate_labels().expect("could not generate ImageNet labels");
}

/// Writes `LABELS` with the short name of each class, e.g. "indigo bunting".
fn generate_labels() -> std::io::Result<()> {
    let out_dir = env::var("OUT_DIR").unwrap();
    let mut out = File::create(Path::new(&out_dir).join("labels.rs"))?;

    writeln!(out, "pub static LABELS: &[&str] = &[")?;
    for line in BufReader::new(File::open(LABEL_SOURCE_FILE)?).lines() {
        let label = short_label(&line?);
        writeln!(out, "    {label:?},")?;
    }
    writeln!(out, "];")?;
    Ok(())
}

/// "n01537544 indigo bunting, indigo finch, ..." -> "indigo bunting".
fn short_label(line: &str) -> String {
    let rest = line.split_once(' ').map_or("", |(_, rest)| rest);
    rest.split(',').next().unwrap_or("").trim().to_string()
}
