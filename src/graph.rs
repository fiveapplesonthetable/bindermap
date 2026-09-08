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

//! The reverse call graph and the class-hierarchy facts the tracer walks: which
//! methods call a given method, each class's transitive ancestors, which concrete
//! methods override a public-API method, and which interfaces are AIDL entrypoints.

use crate::dex::{Class, MethodRef};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

const IINTERFACE: &str = "android/os/IInterface";

/// Everything the reverse trace queries, computed once from the loaded classes and
/// the public-API oracle.
pub struct Index {
    classes: HashMap<String, Class>,
    /// callee -> the methods that invoke it.
    reverse: HashMap<MethodRef, HashSet<MethodRef>>,
    /// class internal name -> its transitive ancestors (supers + interfaces).
    ancestors: HashMap<String, HashSet<String>>,
    /// public SDK method signatures (`owner/name+desc`).
    public_sigs: HashSet<String>,
    /// a concrete method -> the public-API method it overrides/implements.
    virtual_to_public: HashMap<MethodRef, MethodRef>,
}

impl Index {
    pub fn build(classes: Vec<Class>, public_sigs: HashSet<String>) -> Self {
        let mut by_name: HashMap<String, Class> = HashMap::with_capacity(classes.len());
        let mut reverse: HashMap<MethodRef, HashSet<MethodRef>> = HashMap::new();
        for c in classes {
            for m in &c.methods {
                let caller = MethodRef { owner: c.name.clone(), name: m.name.clone(), desc: m.desc.clone() };
                for callee in &m.invokes {
                    reverse.entry(callee.clone()).or_default().insert(caller.clone());
                }
            }
            // First class of a given name wins (duplicates across jars are ignored
            // for hierarchy, but their call edges above are all kept).
            by_name.entry(c.name.clone()).or_insert(c);
        }

        let ancestors: HashMap<String, HashSet<String>> = by_name
            .par_iter()
            .map(|(name, _)| (name.clone(), close_ancestors(name, &by_name)))
            .collect();

        let mut idx = Index { classes: by_name, reverse, ancestors, public_sigs, virtual_to_public: HashMap::new() };
        idx.virtual_to_public = idx.resolve_virtual_to_public();
        idx
    }

    /// AIDL entrypoints: methods declared on an interface that transitively extends
    /// `android.os.IInterface`, excluding constructors and `asBinder`.
    pub fn aidl_methods(&self) -> Vec<MethodRef> {
        self.classes
            .values()
            .filter(|c| c.is_interface() && self.ancestors_of(&c.name).contains(IINTERFACE))
            .flat_map(|c| {
                c.methods
                    .iter()
                    .filter(|m| !m.name.starts_with('<') && m.name != "asBinder")
                    .map(move |m| MethodRef {
                        owner: c.name.clone(),
                        name: m.name.clone(),
                        desc: m.desc.clone(),
                    })
            })
            .collect()
    }

    pub fn callers(&self, m: &MethodRef) -> Option<&HashSet<MethodRef>> {
        self.reverse.get(m)
    }

    pub fn has_callers(&self, m: &MethodRef) -> bool {
        self.reverse.contains_key(m)
    }

    pub fn jar(&self, owner: &str) -> &str {
        self.classes.get(owner).map(|c| c.jar.as_str()).unwrap_or("UNKNOWN")
    }

    pub fn has_class(&self, owner: &str) -> bool {
        self.classes.contains_key(owner)
    }

    pub fn is_synthetic(&self, owner: &str) -> bool {
        self.classes.get(owner).is_some_and(|c| c.is_synthetic())
    }

    /// The methods that construct instances of `owner` — the callers of any of its
    /// constructors. For a synthetic (lambda) class these are its creation sites.
    pub fn constructor_callers(&self, owner: &str) -> Vec<MethodRef> {
        let Some(c) = self.classes.get(owner) else { return Vec::new() };
        let mut out = Vec::new();
        for m in &c.methods {
            if m.name == "<init>" {
                let ctor = MethodRef { owner: owner.to_string(), name: "<init>".to_string(), desc: m.desc.clone() };
                if let Some(callers) = self.reverse.get(&ctor) {
                    out.extend(callers.iter().cloned());
                }
            }
        }
        out
    }

    /// A method that is itself a public SDK method (signature present, and in a
    /// public namespace).
    pub fn is_public(&self, m: &MethodRef) -> bool {
        self.public_sigs.contains(&m.signature()) && is_public_namespace(&m.owner)
    }

    /// The public-API method this concrete method overrides/implements, if any.
    pub fn public_override(&self, m: &MethodRef) -> Option<&MethodRef> {
        self.virtual_to_public.get(m)
    }

    fn ancestors_of(&self, class: &str) -> &HashSet<String> {
        static EMPTY: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
        self.ancestors.get(class).unwrap_or_else(|| EMPTY.get_or_init(HashSet::new))
    }

    /// For every method of every class, record whether it overrides a public-API
    /// method of an ancestor (so the trace can treat reaching it as a public hit).
    fn resolve_virtual_to_public(&self) -> HashMap<MethodRef, MethodRef> {
        self.classes
            .par_iter()
            .flat_map_iter(|(name, c)| {
                c.methods.iter().filter_map(move |m| {
                    self.find_public_parent(name, &m.name, &m.desc).map(|p| {
                        (MethodRef { owner: name.clone(), name: m.name.clone(), desc: m.desc.clone() }, p)
                    })
                })
            })
            .collect()
    }

    /// Walk supers/interfaces looking for an ancestor that declares this method in
    /// the public SDK (and in a public namespace).
    fn find_public_parent(&self, class: &str, name: &str, desc: &str) -> Option<MethodRef> {
        for anc in self.ancestors_of(class) {
            let cand = MethodRef { owner: anc.clone(), name: name.to_string(), desc: desc.to_string() };
            if self.public_sigs.contains(&cand.signature()) && is_public_namespace(anc) {
                return Some(cand);
            }
        }
        None
    }
}

/// Transitive supers + interfaces of `class`, excluding `java/lang/Object`.
fn close_ancestors(class: &str, by_name: &HashMap<String, Class>) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    if let Some(c) = by_name.get(class) {
        if let Some(s) = &c.super_name {
            stack.push(s.clone());
        }
        stack.extend(c.interfaces.iter().cloned());
    }
    while let Some(a) = stack.pop() {
        if a == "java/lang/Object" || !out.insert(a.clone()) {
            continue;
        }
        if let Some(c) = by_name.get(&a) {
            if let Some(s) = &c.super_name {
                stack.push(s.clone());
            }
            stack.extend(c.interfaces.iter().cloned());
        }
    }
    out
}

/// A public-facing namespace: `android.*` or `com.android.*`, excluding the Binder
/// runtime and `com.android.internal`.
pub fn is_public_namespace(owner: &str) -> bool {
    (owner.starts_with("android/") || owner.starts_with("com/android/"))
        && !owner.starts_with("android/os/Binder")
        && !owner.starts_with("com/android/internal/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex::{Method, ACC_INTERFACE};

    fn iface(name: &str, supers: &[&str], methods: &[&str]) -> Class {
        Class {
            name: name.into(),
            super_name: Some("java/lang/Object".into()),
            interfaces: supers.iter().map(|s| s.to_string()).collect(),
            access: ACC_INTERFACE,
            methods: methods.iter().map(|m| Method { name: m.to_string(), desc: "()V".into(), invokes: vec![] }).collect(),
            jar: "svc.jar".into(),
        }
    }

    #[test]
    fn discovers_aidl_via_iinterface() {
        let classes = vec![
            iface("com/x/IFoo", &[IINTERFACE], &["doThing", "asBinder", "<clinit>"]),
            // a Stub is a class implementing IFoo — not itself an AIDL interface entry.
            Class { name: "com/x/IFoo$Stub".into(), super_name: Some("android/os/Binder".into()),
                    interfaces: vec!["com/x/IFoo".into()], access: 0, methods: vec![], jar: "svc.jar".into() },
        ];
        let idx = Index::build(classes, HashSet::new());
        let aidl: Vec<String> = idx.aidl_methods().iter().map(|m| m.name.clone()).collect();
        assert_eq!(aidl, vec!["doThing"]); // asBinder + <clinit> excluded, Stub excluded
    }

    #[test]
    fn reverse_edges_and_public_override() {
        let mut caller = Class {
            name: "com/android/Svc".into(),
            super_name: Some("android/x/Base".into()),
            interfaces: vec![],
            access: 0,
            methods: vec![Method { name: "run".into(), desc: "()V".into(),
                invokes: vec![MethodRef { owner: "com/x/IFoo".into(), name: "doThing".into(), desc: "()V".into() }] }],
            jar: "svc.jar".into(),
        };
        caller.methods.push(Method { name: "onCmd".into(), desc: "()V".into(), invokes: vec![] });
        let base = Class { name: "android/x/Base".into(), super_name: None, interfaces: vec![],
            access: 0, methods: vec![Method { name: "onCmd".into(), desc: "()V".into(), invokes: vec![] }], jar: "fw.jar".into() };
        let public = HashSet::from(["android/x/Base.onCmd()V".to_string()]);
        let idx = Index::build(vec![caller, base], public);

        let callee = MethodRef { owner: "com/x/IFoo".into(), name: "doThing".into(), desc: "()V".into() };
        let callers = idx.callers(&callee).unwrap();
        assert!(callers.contains(&MethodRef { owner: "com/android/Svc".into(), name: "run".into(), desc: "()V".into() }));

        // Svc.onCmd overrides the public Base.onCmd.
        let svc_oncmd = MethodRef { owner: "com/android/Svc".into(), name: "onCmd".into(), desc: "()V".into() };
        assert_eq!(idx.public_override(&svc_oncmd).map(|p| p.owner.clone()), Some("android/x/Base".into()));
    }
}
