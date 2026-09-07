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

//! Output. Purely a serializer of the analysis result — it holds no graph or dex
//! state, so rendering (CSV/JSON here, HTML in a separate tool) stays decoupled
//! from tracing.

use crate::trace::Mapping;
use anyhow::{Context, Result};
use std::path::Path;

/// The full analysis result, ready to serialize.
#[derive(serde::Serialize)]
pub struct Report {
    pub mappings: Vec<Mapping>,
    pub unmapped: Vec<Unmapped>,
    /// per-AIDL-method call chain, for the debug log (sorted by `fqn`).
    #[serde(skip)]
    pub debug: Vec<DebugEntry>,
}

#[derive(Debug, serde::Serialize)]
pub struct Unmapped {
    pub aidl: String,
    pub reason: String,
}

pub struct DebugEntry {
    pub fqn: String,
    pub mapped: bool,
    pub path: String,
}

impl Report {
    /// Write `binder_mapping.csv`, `unmapped_aidl.csv`, `trace_debug.log`, and
    /// `binder_mapping.json` into `dir`. The CSV columns match the reference tool so
    /// an external HTML renderer consumes them unchanged.
    pub fn write(&self, dir: &Path, suffix: &str) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;

        let mut w = csv::Writer::from_path(dir.join(format!("binder_mapping{suffix}.csv")))?;
        w.write_record(["BINDER_INTERFACE_METHOD", "PUBLIC_API_CLASS", "METHOD", "DESC", "HOPS", "JAR"])?;
        for m in &self.mappings {
            w.write_record([&m.aidl, &m.public_class, &m.method, &m.desc, &m.hops.to_string(), &m.jar])?;
        }
        w.flush()?;

        let mut w = csv::Writer::from_path(dir.join(format!("unmapped_aidl{suffix}.csv")))?;
        w.write_record(["AIDL_METHOD", "REASON"])?;
        for u in &self.unmapped {
            w.write_record([&u.aidl, &u.reason])?;
        }
        w.flush()?;

        let mut log = String::new();
        for e in &self.debug {
            let status = if e.mapped { "[MAPPED]" } else { "[UNMAPPED]" };
            log.push_str(&format!("{status} {}\n    PATH:\n{}\n", e.fqn, e.path));
        }
        std::fs::write(dir.join(format!("trace_debug{suffix}.log")), log)?;

        std::fs::write(
            dir.join(format!("binder_mapping{suffix}.json")),
            serde_json::to_string_pretty(self)?,
        )?;
        Ok(())
    }
}
