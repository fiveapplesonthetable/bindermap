// Copyright (C) 2026 The Android Open Source Project
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! bindermap CLI.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "bindermap", about = "Reverse-trace AIDL/binder methods to the public SDK APIs that reach them")]
struct Args {
    /// The public-API oracle: a signature list (`.txt`), or an SDK dex/jar whose
    /// methods are taken as public.
    #[arg(long)]
    public_api: PathBuf,

    /// Framework DEX inputs: `.dex` / `.jar` / `.apk` files or directories.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Find every public API in each call chain (comprehensive) rather than
    /// stopping at the first (fast, default).
    #[arg(long)]
    all_paths: bool,

    /// Output directory for the CSV/JSON/log reports.
    #[arg(long, default_value = ".")]
    out_dir: PathBuf,

    /// Path to `dexdump` (else `$BINDERMAP_DEXDUMP`, else `PATH`).
    #[arg(long)]
    dexdump: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(d) = &args.dexdump {
        std::env::set_var("BINDERMAP_DEXDUMP", d);
    }

    let report = bindermap::run(&args.inputs, &args.public_api, args.all_paths)?;
    let suffix = if args.all_paths { "_all" } else { "" };
    report.write(&args.out_dir, suffix)?;

    eprintln!(
        "[bindermap] {} mapping line(s), {} AIDL method(s) unmapped; wrote binder_mapping{}.csv to {}",
        report.mappings.len(),
        report.unmapped.len(),
        suffix,
        args.out_dir.display()
    );
    Ok(())
}
