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

//! The public-API oracle: the set of method signatures (`owner/name+desc`) that
//! count as public SDK entrypoints. Loaded either from a signature list (one per
//! line) or from an SDK dex/jar whose every method is taken as public.

use crate::dex;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;

/// Load the oracle. A `.txt`/`.list` file is read as one signature per line
/// (`#` comments and blanks ignored); any other input is decoded as DEX and every
/// method signature it declares is taken as public.
pub fn load(path: &Path) -> Result<HashSet<String>> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("txt") | Some("list") | Some("sig") => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading API list {}", path.display()))?;
            Ok(text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(normalize)
                .collect())
        }
        _ => {
            let classes = dex::read(std::slice::from_ref(&path.to_path_buf()))
                .with_context(|| format!("decoding SDK {}", path.display()))?;
            Ok(classes
                .iter()
                .flat_map(|c| {
                    c.methods.iter().map(move |m| format!("{}.{}{}", c.name, m.name, m.desc))
                })
                .collect())
        }
    }
}

/// Accept either internal (`owner/name+desc`) or dotted (`owner.name+desc`) owners
/// in a signature line, storing the internal form the graph keys on.
fn normalize(line: &str) -> String {
    let Some((before_paren, params)) = line.split_once('(') else {
        return line.replace('.', "/");
    };
    // The last '.' before '(' separates the method name from the owner.
    match before_paren.rsplit_once('.') {
        Some((owner, method)) => format!("{}.{}({}", owner.replace('.', "/"), method, params),
        None => line.replace('.', "/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_owner_form() {
        assert_eq!(normalize("android.content.Context.bindService(Landroid/content/Intent;)Z"),
                   "android/content/Context.bindService(Landroid/content/Intent;)Z");
        assert_eq!(normalize("android/content/Context.bindService()Z"),
                   "android/content/Context.bindService()Z");
    }
}
