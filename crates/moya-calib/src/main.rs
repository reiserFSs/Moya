mod io;
mod generate;
mod compare;

use anyhow::{Result, bail, Context};
use std::{path::PathBuf, process::Command, time::Instant};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference-corpus/ops");
    let (mut op, mut case, mut dump) = (None, None, false);
    let mut json_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = args.next().context("--root requires a path")?.into(),
            "--op" => op = Some(args.next().context("--op requires a name")?),
            "--case" => case = Some(args.next().context("--case requires an id")?),
            "--dump" => dump = true,
            "--json" => json_path = Some(args.next().context("--json requires a path")?.into()),
            _ => bail!("unknown argument {arg}"),
        }
    }
    match command.as_str() {
        "gen" => generate::generate(&root),
        "compare" => compare::compare(&root, op.as_deref(), case.as_deref(), dump, json_path.as_deref()),
        "capture" => {
            let root = std::fs::canonicalize(root)?;
            let start = Instant::now();
            let runner = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../calib/runner.jsx");
            let wrapper = std::env::temp_dir().join(format!("moya-capture-{}.jsx", std::process::id()));
            std::fs::write(&wrapper, format!("var MOYA_ROOT={}; var MOYA_OP={}; var MOYA_CASE={}; $.evalFile(new File({}));", serde_json::to_string(&root.to_string_lossy())?, serde_json::to_string(&op)?, serde_json::to_string(&case)?, serde_json::to_string(&runner.to_string_lossy())?))?;
            let bridge = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../calib/ps-run.sh");
            let status = Command::new("sh").arg(bridge).arg(&wrapper).status();
            let _ = std::fs::remove_file(wrapper);
            anyhow::ensure!(status?.success(), "Photoshop capture failed");
            println!("Capture wall time: {:.3}s", start.elapsed().as_secs_f64());
            Ok(())
        },
        _ => { println!("moya-calib gen|capture|compare [--root PATH] [--op NAME] [--case ID] [--dump] [--json PATH]"); Ok(()) }
    }
}
