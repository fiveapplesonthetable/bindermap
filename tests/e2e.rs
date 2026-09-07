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

//! End-to-end trace over a prebuilt DEX fixture: an AIDL interface `IFoo` whose
//! method is reached from a public API through a helper, plus a server `$Stub` the
//! tracer must skip. Runs only when `dexdump` is available (env `BINDERMAP_DEXDUMP`
//! or on `PATH`); skipped otherwise so `cargo test` stays self-contained.

use std::path::{Path, PathBuf};
use std::process::Command;

fn dexdump_available() -> bool {
    if let Ok(p) = std::env::var("BINDERMAP_DEXDUMP") {
        return Path::new(&p).exists();
    }
    Command::new("dexdump").arg("-v").output().is_ok()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn reverse_traces_aidl_to_public_api() {
    if !dexdump_available() {
        eprintln!("skipping e2e: dexdump not found (set BINDERMAP_DEXDUMP)");
        return;
    }
    let dir = fixtures();
    let report = bindermap::run(&[dir.join("scenario.dex")], &dir.join("scenario_sdk.txt"), false)
        .expect("analysis");

    // IFoo.doThing reaches the public API PublicApi.publicEntry via Helper.step;
    // the IFoo$Stub.onTransact caller is skipped (server dispatch).
    assert_eq!(report.mappings.len(), 1, "mappings: {:?}", report.mappings);
    let m = &report.mappings[0];
    assert_eq!(m.aidl, "com.example.IFoo.doThing");
    assert_eq!(m.public_class, "android.app.PublicApi");
    assert_eq!(m.method, "publicEntry");
    assert_eq!(m.hops, 2);
    assert!(report.unmapped.is_empty(), "unexpected unmapped: {:?}", report.unmapped);
}
