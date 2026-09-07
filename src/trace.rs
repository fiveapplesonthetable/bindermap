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

//! Reverse breadth-first trace: from an AIDL method, walk the reverse call graph
//! outward until a public SDK method is reached, recording the call chain.

use crate::dex::MethodRef;
use crate::graph::{is_public_namespace, Index};
use std::collections::{HashSet, VecDeque};

const MAX_HOPS: usize = 50;

/// One AIDL-method → public-API mapping.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Mapping {
    pub aidl: String,
    pub public_class: String,
    pub method: String,
    pub desc: String,
    pub hops: usize,
    pub jar: String,
}

/// The result of tracing one AIDL method.
pub struct Trace {
    pub mappings: Vec<Mapping>,
    /// the call chain (public → AIDL), rendered for the debug report.
    pub path: String,
    pub mapped: bool,
}

struct Node {
    sig: MethodRef,
    hops: usize,
    parent: Option<usize>,
}

/// Trace one AIDL method back to the public API. With `all_paths`, keep exploring
/// past a public hit to find every public entrypoint; otherwise stop each branch at
/// the first hit.
pub fn trace(idx: &Index, start: &MethodRef, all_paths: bool) -> Trace {
    let mut arena: Vec<Node> = vec![Node { sig: start.clone(), hops: 0, parent: None }];
    let mut queue: VecDeque<usize> = VecDeque::from([0]);
    let mut visited: HashSet<String> = HashSet::new();
    let mut mappings: Vec<Mapping> = Vec::new();
    let mut first_path: Option<String> = None;
    let mut deepest: usize = 0;

    while let Some(ni) = queue.pop_front() {
        let (sig, hops) = { let n = &arena[ni]; (n.sig.clone(), n.hops) };
        if !visited.insert(sig.signature()) {
            continue;
        }
        if hops > MAX_HOPS {
            continue;
        }
        if arena[ni].hops >= arena[deepest].hops {
            deepest = ni;
        }
        let mut hit = false;

        // Direct public SDK method.
        if idx.is_public(&sig) {
            mappings.push(Mapping {
                aidl: start.fqn(),
                public_class: sig.owner.replace('/', "."),
                method: sig.name.clone(),
                desc: sig.desc.clone(),
                hops,
                jar: idx.jar(&sig.owner).to_string(),
            });
            first_path.get_or_insert_with(|| render_path(&arena, ni, idx));
            hit = true;
        }

        // Overrides/implements a public-API method.
        if let Some(pubp) = idx.public_override(&sig) {
            if is_public_namespace(&pubp.owner) {
                mappings.push(Mapping {
                    aidl: start.fqn(),
                    public_class: pubp.owner.replace('/', "."),
                    method: pubp.name.clone(),
                    desc: pubp.desc.clone(),
                    hops,
                    jar: idx.jar(&sig.owner).to_string(),
                });
                first_path.get_or_insert_with(|| {
                    format!("{}      -> [VIRTUAL] {}\n", render_path(&arena, ni, idx), pubp.fqn())
                });
                hit = true;
            }
        }

        if hit && !all_paths {
            continue;
        }

        // Expand: callers of this method (skip server-side $Stub dispatch).
        if let Some(callers) = idx.callers(&sig) {
            for caller in callers {
                if is_server_stub(&caller.owner) {
                    continue;
                }
                let child = arena.len();
                arena.push(Node { sig: caller.clone(), hops: hops + 1, parent: Some(ni) });
                queue.push_back(child);
            }
        }
        // Bridge an inner class to its enclosing class (same method, no hop cost),
        // but only to a class that actually exists — so a synthetic name never
        // spawns a dead node.
        if let Some(outer) = enclosing(&sig.owner) {
            if idx.has_class(&outer) {
                let outer_sig = MethodRef { owner: outer, name: sig.name.clone(), desc: sig.desc.clone() };
                let child = arena.len();
                arena.push(Node { sig: outer_sig, hops, parent: Some(ni) });
                queue.push_back(child);
            }
        }
    }

    let mapped = !mappings.is_empty();
    let path = if mapped {
        first_path.unwrap_or_default()
    } else {
        format!("{}      [DEAD_END]\n", render_path(&arena, deepest, idx))
    };
    Trace { mappings, path, mapped }
}

/// Server-side binder stub (`$Stub`, but not the client `$Stub$Proxy`): its
/// `onTransact` dispatch traces back through the binder runtime into generic hits.
fn is_server_stub(owner: &str) -> bool {
    owner.ends_with("$Stub") || (owner.contains("$Stub$") && !owner.contains("$Stub$Proxy"))
}

/// The enclosing class of an inner class. D8 synthetic classes use a `$$` marker
/// (`com/x/Outer$$ExternalSyntheticLambda0`) whose enclosing type is the part
/// before it; ordinary nesting (`com/x/Outer$Inner`) splits on the last `$`.
fn enclosing(owner: &str) -> Option<String> {
    if let Some((outer, _)) = owner.split_once("$$") {
        return Some(outer.to_string());
    }
    owner.rsplit_once('$').map(|(outer, _)| outer.to_string())
}

/// Render the call chain from the AIDL start down to `node`, one indented step per
/// hop, matching the debug-log format the HTML renderer parses.
fn render_path(arena: &[Node], node: usize, idx: &Index) -> String {
    let mut chain: Vec<&MethodRef> = Vec::new();
    let mut cur = Some(node);
    while let Some(i) = cur {
        chain.push(&arena[i].sig);
        cur = arena[i].parent;
    }
    chain.reverse();
    let mut s = String::new();
    for (i, sig) in chain.iter().enumerate() {
        let jar = idx.jar(&sig.owner);
        let jar_tag = if jar == "UNKNOWN" { String::new() } else { format!(" [{jar}]") };
        s.push_str(&"  ".repeat(i));
        s.push_str(&format!("      {i}. {}{}\n", sig.fqn(), jar_tag));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_stub_detection() {
        assert!(is_server_stub("com/x/IFoo$Stub"));
        assert!(!is_server_stub("com/x/IFoo$Stub$Proxy"));
        assert!(!is_server_stub("com/x/Foo"));
    }

    #[test]
    fn enclosing_class() {
        assert_eq!(enclosing("com/x/Outer$Inner").as_deref(), Some("com/x/Outer"));
        assert_eq!(enclosing("com/x/Outer"), None);
        // D8 synthetic lambda: enclosing is the type before `$$`, not `com/x/Outer$`.
        assert_eq!(enclosing("com/x/Outer$$ExternalSyntheticLambda0").as_deref(), Some("com/x/Outer"));
    }
}
