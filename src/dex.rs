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

//! Minimal DEX front-end for reverse call-graph tracing: parse `dexdump -d` into a
//! class graph (name, superclass, interfaces, access) plus, per method, the set of
//! methods it invokes. Only what the binder→API tracer needs is modeled; lock and
//! field detail is ignored.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `ACC_INTERFACE` — set on interface classes, so an AIDL interface is told apart
/// from its `$Stub` / `$Stub$Proxy` implementations.
pub const ACC_INTERFACE: u32 = 0x0200;

/// A method reference in internal (slashed) form: `owner` = `com/foo/Bar`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MethodRef {
    pub owner: String,
    pub name: String,
    pub desc: String,
}

impl MethodRef {
    /// `owner.name+desc` — the identity used across the call graph.
    pub fn signature(&self) -> String {
        format!("{}.{}{}", self.owner, self.name, self.desc)
    }
    /// `com.foo.Bar.name` — dotted, for display.
    pub fn fqn(&self) -> String {
        format!("{}.{}", self.owner.replace('/', "."), self.name)
    }
}

#[derive(Debug, Clone)]
pub struct Method {
    pub name: String,
    pub desc: String,
    /// methods this one invokes (declared targets, in call order; deduped).
    pub invokes: Vec<MethodRef>,
}

#[derive(Debug, Clone)]
pub struct Class {
    /// internal (slashed) name, e.g. `com/foo/Bar`.
    pub name: String,
    pub super_name: Option<String>,
    pub interfaces: Vec<String>,
    pub access: u32,
    pub methods: Vec<Method>,
    /// the jar/apk this class was read from.
    pub jar: String,
}

impl Class {
    pub fn is_interface(&self) -> bool {
        self.access & ACC_INTERFACE != 0
    }
}

/// Locate the `dexdump` binary: `$BINDERMAP_DEXDUMP`, else `dexdump` on `PATH`.
fn dexdump_bin() -> String {
    std::env::var("BINDERMAP_DEXDUMP").unwrap_or_else(|_| "dexdump".to_string())
}

/// Read every `.jar` / `.apk` / `.dex` under `paths` (files or directories) into a
/// flat class list. Jars are scanned for `classes*.dex`; each dex is decoded once.
/// Work is parallelized across inputs.
pub fn read(paths: &[PathBuf]) -> Result<Vec<Class>> {
    let mut inputs: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            for e in walkdir::WalkDir::new(p).into_iter().flatten() {
                let q = e.path();
                if is_dex_bearing(q) {
                    inputs.push(q.to_path_buf());
                }
            }
        } else if is_dex_bearing(p) {
            inputs.push(p.clone());
        }
    }
    let nested: Vec<Vec<Class>> = inputs
        .par_iter()
        .map(|p| read_one(p).unwrap_or_default())
        .collect();
    Ok(nested.into_iter().flatten().collect())
}

fn is_dex_bearing(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()),
        Some("jar") | Some("apk") | Some("dex")
    )
}

/// Read one input (a `.dex`, or a `.jar`/`.apk` whose `classes*.dex` are decoded).
fn read_one(path: &Path) -> Result<Vec<Class>> {
    let jar = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext == "dex" {
        return Ok(tag(parse(&dexdump(path)?), &jar));
    }
    // Extract classes*.dex in-process and decode each.
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .with_context(|| format!("opening {}", path.display()))?;
    let tmp = tempdir(&jar)?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let name = e.name().to_string();
        if !(name.starts_with("classes") && name.ends_with(".dex")) {
            continue;
        }
        let mut buf = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut buf)?;
        let dex_path = tmp.join(name.replace('/', "_"));
        std::fs::write(&dex_path, &buf)?;
        out.extend(tag(parse(&dexdump(&dex_path)?), &jar));
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(out)
}

fn tag(mut classes: Vec<Class>, jar: &str) -> Vec<Class> {
    for c in &mut classes {
        c.jar = jar.to_string();
    }
    classes
}

fn tempdir(tag: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("bindermap-{}-{}", sanitize(tag), std::process::id()));
    for n in 0..u32::MAX {
        let p = base.with_extension(n.to_string());
        if std::fs::create_dir_all(&p).is_ok() {
            return Ok(p);
        }
    }
    anyhow::bail!("could not create a temp dir for {tag}")
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn dexdump(dex: &Path) -> Result<String> {
    let out = Command::new(dexdump_bin())
        .arg("-d")
        .arg(dex)
        .output()
        .with_context(|| format!("running dexdump on {}", dex.display()))?;
    anyhow::ensure!(out.status.success(), "dexdump failed on {}", dex.display());
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Which subsection of a class body the parser is inside.
#[derive(PartialEq)]
enum Sec {
    Head,
    Interfaces,
    Fields,
    Methods,
}

/// Parse `dexdump -d` text into classes. Per class:
/// - `Class descriptor  : 'Lcom/foo/Bar;'`, `Access flags : 0x0601 (...)`,
///   `Superclass : 'L...;'`, `Interfaces` then `#0 : 'L...;'`.
/// - In `Direct/Virtual methods`, each method is a `name : '...'` + `type : 'desc'`
///   pair (present even for abstract/interface methods with no code).
/// - Invoke edges come from the disassembly body lines (`<addr>| <n>: invoke-* ...,
///   Lowner;.m:desc`) and attach to the method whose declaration last appeared.
pub fn parse(text: &str) -> Vec<Class> {
    let mut classes: Vec<Class> = Vec::new();
    let mut cur: Option<Class> = None;
    let mut sec = Sec::Head;
    let mut pending_name: Option<String> = None;

    for raw in text.lines() {
        let line = raw.trim_end();
        let t = line.trim_start();

        if let Some(rest) = t.strip_prefix("Class descriptor") {
            if let Some(c) = cur.take() {
                classes.push(c);
            }
            sec = Sec::Head;
            pending_name = None;
            cur = Some(Class {
                name: internal(&quoted(rest)),
                super_name: None,
                interfaces: Vec::new(),
                access: 0,
                methods: Vec::new(),
                jar: String::new(),
            });
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };

        // Section transitions. `Access flags` (capitalized, spaced) is class-level
        // only; method/field access lines use lowercase `access`.
        if let Some(rest) = t.strip_prefix("Access flags") {
            c.access = hex_after_colon(rest);
            continue;
        }
        if let Some(rest) = t.strip_prefix("Superclass") {
            c.super_name = Some(internal(&quoted(rest)));
            continue;
        }
        if t.starts_with("Interfaces") {
            sec = Sec::Interfaces;
            continue;
        }
        if t.starts_with("Static fields") || t.starts_with("Instance fields") {
            sec = Sec::Fields;
            continue;
        }
        if t.starts_with("Direct methods") || t.starts_with("Virtual methods") {
            sec = Sec::Methods;
            continue;
        }

        match sec {
            Sec::Interfaces if t.starts_with('#') && t.contains('\'') => {
                c.interfaces.push(internal(&quoted(t)));
            }
            Sec::Methods => {
                // Declaration: `name : '...'` then `type : '(..)ret'` creates the method.
                if let Some(rest) = t.strip_prefix("name") {
                    pending_name = Some(quoted(rest));
                } else if let Some(rest) = t.strip_prefix("type") {
                    if let Some(name) = pending_name.take() {
                        c.methods.push(Method { name, desc: quoted(rest), invokes: Vec::new() });
                    }
                } else if let Some(idx) = line.find('|') {
                    // Disassembly: attach any invoke to the method last declared.
                    if let (Some(m), Some(target)) =
                        (c.methods.last_mut(), invoke_target(line[idx + 1..].trim_start()))
                    {
                        if !m.invokes.contains(&target) {
                            m.invokes.push(target);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(c) = cur.take() {
        classes.push(c);
    }
    classes
}

/// Extract the callee of an `invoke-*` instruction body, else `None`.
/// `0006: invoke-interface {v0}, Lcom/foo/IBar;.baz:(I)V // method@123`
fn invoke_target(body: &str) -> Option<MethodRef> {
    let after_colon = body.split_once(':')?.1.trim_start();
    if !after_colon.starts_with("invoke-") {
        return None;
    }
    // The method ref is the last comma-separated operand, before any ` // ` comment.
    let ref_part = after_colon.rsplit_once(',')?.1.trim();
    let ref_part = ref_part.split(" // ").next().unwrap_or(ref_part).trim();
    parse_method_ref(ref_part)
}

/// `Lcom/foo/Bar;.name:desc` -> MethodRef.
fn parse_method_ref(s: &str) -> Option<MethodRef> {
    let s = s.strip_prefix('L')?;
    let (owner, rest) = s.split_once(";.")?;
    let (name, desc) = rest.split_once(':')?;
    Some(MethodRef {
        owner: owner.to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
    })
}

/// `'Lcom/foo/Bar;'` -> `com/foo/Bar` (internal name).
fn internal(quoted_desc: &str) -> String {
    let d = quoted_desc.trim();
    d.strip_prefix('L').and_then(|s| s.strip_suffix(';')).unwrap_or(d).to_string()
}

/// Text between the first pair of single quotes.
fn quoted(s: &str) -> String {
    let mut it = s.split('\'');
    it.next();
    it.next().unwrap_or("").to_string()
}

/// Parse the hex value after a `:` (e.g. `Access flags : 0x0601 (...)`).
fn hex_after_colon(s: &str) -> u32 {
    let after = s.split_once(':').map(|(_, r)| r).unwrap_or(s).trim();
    let tok = after.split_whitespace().next().unwrap_or("");
    tok.strip_prefix("0x")
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_method_ref() {
        let r = parse_method_ref("Lcom/foo/Bar;.baz:(I)V").unwrap();
        assert_eq!(r.owner, "com/foo/Bar");
        assert_eq!(r.name, "baz");
        assert_eq!(r.desc, "(I)V");
        assert_eq!(r.signature(), "com/foo/Bar.baz(I)V");
        assert_eq!(r.fqn(), "com.foo.Bar.baz");
    }

    #[test]
    fn extracts_invoke_callee() {
        let t = invoke_target("0006: invoke-interface {v0}, Lcom/foo/IBar;.f:(I)V // method@1").unwrap();
        assert_eq!(t.owner, "com/foo/IBar");
        assert_eq!(t.name, "f");
        assert_eq!(t.desc, "(I)V");
        assert!(invoke_target("0002: const/4 v0, #int 0").is_none());
    }

    #[test]
    fn internal_and_hex() {
        assert_eq!(internal(&quoted("descriptor : 'Lcom/foo/Bar;'")), "com/foo/Bar");
        assert_eq!(hex_after_colon(" : 0x0601 (PUBLIC INTERFACE ABSTRACT)"), 0x0601);
    }

    #[test]
    fn parses_class_with_abstract_and_code_methods() {
        // Minimal shape of `dexdump -d`: an interface with an abstract method (no
        // code) and a class whose method has an invoke in its disassembly.
        let text = "\
Class #0            -
  Class descriptor  : 'Lcom/x/IFoo;'
  Access flags      : 0x0601 (PUBLIC INTERFACE ABSTRACT)
  Superclass        : 'Ljava/lang/Object;'
  Interfaces        -
    #0              : 'Landroid/os/IInterface;'
  Static fields     -
  Instance fields   -
  Direct methods    -
  Virtual methods   -
    #0              : (in Lcom/x/IFoo;)
      name          : 'doThing'
      type          : '()V'
      access        : 0x0401 (PUBLIC ABSTRACT)
  source_file_idx   : 1 (IFoo.java)

Class #1            -
  Class descriptor  : 'Lcom/x/Helper;'
  Access flags      : 0x0001 (PUBLIC)
  Superclass        : 'Ljava/lang/Object;'
  Direct methods    -
  Virtual methods   -
    #0              : (in Lcom/x/Helper;)
      name          : 'step'
      type          : '()V'
      access        : 0x0001 (PUBLIC)
      code          -
000100:                                        |[000100] com.x.Helper.step:()V
000110: 6e10 0100 0000                         |0000: invoke-interface {v0}, Lcom/x/IFoo;.doThing:()V // method@0001
000116: 0e00                                   |0003: return-void
";
        let classes = parse(text);
        assert_eq!(classes.len(), 2);
        let ifoo = classes.iter().find(|c| c.name == "com/x/IFoo").unwrap();
        assert!(ifoo.is_interface());
        assert_eq!(ifoo.interfaces, vec!["android/os/IInterface"]);
        assert_eq!(ifoo.methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["doThing"]);
        let helper = classes.iter().find(|c| c.name == "com/x/Helper").unwrap();
        let step = &helper.methods[0];
        assert_eq!(step.name, "step");
        assert_eq!(step.invokes, vec![MethodRef { owner: "com/x/IFoo".into(), name: "doThing".into(), desc: "()V".into() }]);
    }
}
