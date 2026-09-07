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

//! bindermap — reverse-trace AIDL/binder interface methods to the public SDK
//! methods that reach them, from DEX bytecode.
//!
//! Layers, each independent:
//! - [`dex`] decodes `dexdump` output into a class graph.
//! - [`graph`] builds the reverse call graph and hierarchy facts.
//! - [`trace`] walks it backwards from each AIDL method to the public API.
//! - [`api`] loads the public-API oracle.
//! - [`report`] serializes the result (CSV/JSON); rendering lives outside.

pub mod api;
pub mod dex;
pub mod graph;
pub mod report;
pub mod trace;

use anyhow::Result;
use graph::Index;
use rayon::prelude::*;
use report::{DebugEntry, Report, Unmapped};
use std::collections::HashSet;
use std::path::PathBuf;

/// Run the full analysis: decode inputs, load the oracle, build the index, trace
/// every AIDL method (in parallel), and assemble the deduped, sorted [`Report`].
pub fn run(inputs: &[PathBuf], oracle: &std::path::Path, all_paths: bool) -> Result<Report> {
    let classes = dex::read(inputs)?;
    let public = api::load(oracle)?;
    let idx = Index::build(classes, public);

    let aidl = idx.aidl_methods();
    let traces: Vec<(String, bool, bool, String, Vec<trace::Mapping>)> = aidl
        .par_iter()
        .map(|a| {
            let t = trace::trace(&idx, a, all_paths);
            (a.fqn(), t.mapped, idx.has_callers(a), t.path, t.mappings)
        })
        .collect();

    // Dedup mappings across AIDL methods by their full CSV identity.
    let mut seen: HashSet<(String, String, String, String, usize, String)> = HashSet::new();
    let mut mappings: Vec<trace::Mapping> = Vec::new();
    for (_, _, _, _, ms) in &traces {
        for m in ms {
            let key = (m.aidl.clone(), m.public_class.clone(), m.method.clone(), m.desc.clone(), m.hops, m.jar.clone());
            if seen.insert(key) {
                mappings.push(m.clone());
            }
        }
    }
    mappings.sort_by(|a, b| {
        (&a.aidl, &a.public_class, &a.method, &a.desc, a.hops, &a.jar)
            .cmp(&(&b.aidl, &b.public_class, &b.method, &b.desc, b.hops, &b.jar))
    });

    let mut unmapped: Vec<Unmapped> = Vec::new();
    let mut debug: Vec<DebugEntry> = Vec::new();
    for (fqn, mapped, has_callers, path, _) in traces {
        if !mapped {
            let reason = if has_callers { "NO_PUBLIC_PATH" } else { "NO_CALLERS_FOUND" };
            unmapped.push(Unmapped { aidl: fqn.clone(), reason: reason.to_string() });
        }
        debug.push(DebugEntry { fqn, mapped, path });
    }
    unmapped.sort_by(|a, b| a.aidl.cmp(&b.aidl));
    debug.sort_by(|a, b| a.fqn.cmp(&b.fqn));

    Ok(Report { mappings, unmapped, debug })
}
