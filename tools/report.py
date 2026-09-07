#!/usr/bin/env python3
# Copyright (C) 2026 The Android Open Source Project
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
# WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.

"""Render bindermap's CSV/log output into a single filterable HTML page.

Pure consumer of the analysis output — it reads `binder_mapping.csv`,
`unmapped_aidl.csv`, and `trace_debug.log` and writes `report.html`. It shares no
state with the analyzer. SDK methods link to the public developer reference; there
are no environment-specific links.

Usage: python3 report.py [DIR]   (DIR defaults to the current directory)
"""
import csv
import html
import os
import re
import sys


def parse_paths(debug_log):
    """Map each AIDL fqn to its cleaned call chain from the debug log."""
    paths = {}
    if not os.path.exists(debug_log):
        return paths
    text = open(debug_log, encoding="utf-8").read()
    parts = re.split(r"(\[(?:MAPPED|UNMAPPED)\]\s+)", text)
    for i in range(1, len(parts), 2):
        body = parts[i + 1]
        fqn = body.split("\n", 1)[0].strip()
        m = re.search(r"PATH:\n(.*?)(?=\n\[|$)", body, re.DOTALL)
        if not m:
            continue
        lines = []
        for line in m.group(1).splitlines():
            line = re.sub(r"^\s*\d+\.\s*", "", line.strip())
            line = line.replace("[DEAD_END]", "").strip()
            if line:
                lines.append(line)
        paths[fqn] = "\n".join(lines)
    return paths


def sdk_url(cls, method):
    ref = cls.replace(".", "/")
    return f"https://developer.android.com/reference/{ref}#{method}"


def rows(path):
    return list(csv.DictReader(open(path, encoding="utf-8"))) if os.path.exists(path) else []


def render(directory):
    mapping = rows(os.path.join(directory, "binder_mapping.csv"))
    unmapped = rows(os.path.join(directory, "unmapped_aidl.csv"))
    paths = parse_paths(os.path.join(directory, "trace_debug.log"))

    def esc(s):
        return html.escape(s or "", quote=True)

    mapped_html = []
    for i, r in enumerate(mapping, 1):
        aidl = r["BINDER_INTERFACE_METHOD"]
        cls, method = r["PUBLIC_API_CLASS"], r["METHOD"]
        stack = paths.get(aidl, "")
        mapped_html.append(
            f"<tr data-aidl='{esc(aidl)}' data-sdk='{esc(cls + '.' + method)}' "
            f"data-hops='{esc(r['HOPS'])}' data-jar='{esc(r['JAR'])}' data-stack='{esc(stack)}'>"
            f"<td class='idx'>{i}</td>"
            f"<td>{esc(aidl)}</td>"
            f"<td><a href='{esc(sdk_url(cls, method))}' target='_blank'>{esc(cls)}.{esc(method)}</a>"
            f"<div class='desc'>{esc(r['DESC'])}</div></td>"
            f"<td class='ctr'>{esc(r['HOPS'])}</td>"
            f"<td class='jar'>{esc(r['JAR'])}</td>"
            f"<td class='stack'>{esc(stack)}</td></tr>"
        )

    unmapped_html = []
    for i, r in enumerate(unmapped, 1):
        aidl = r["AIDL_METHOD"]
        unmapped_html.append(
            f"<tr data-aidl='{esc(aidl)}' data-reason='{esc(r['REASON'])}' "
            f"data-stack='{esc(paths.get(aidl, ''))}'>"
            f"<td class='idx'>{i}</td><td>{esc(aidl)}</td>"
            f"<td>{esc(r['REASON'])}</td>"
            f"<td class='stack'>{esc(paths.get(aidl, ''))}</td></tr>"
        )

    out = os.path.join(directory, "report.html")
    open(out, "w", encoding="utf-8").write(
        _PAGE.replace("{{N_MAPPED}}", str(len(mapping)))
        .replace("{{N_UNMAPPED}}", str(len(unmapped)))
        .replace("{{MAPPED_ROWS}}", "\n".join(mapped_html))
        .replace("{{UNMAPPED_ROWS}}", "\n".join(unmapped_html))
    )
    print(f"wrote {out} ({len(mapping)} mapped, {len(unmapped)} unmapped)")


_PAGE = """<!DOCTYPE html><html><head><meta charset="utf-8"><title>bindermap</title><style>
 body{font-family:system-ui,sans-serif;margin:0;color:#222}
 header{background:#111;color:#fff;padding:12px 18px;font-weight:600}
 .tabs{display:flex;background:#222}
 .tabs a{color:#bbb;padding:10px 18px;cursor:pointer;text-decoration:none;font-size:13px}
 .tabs a.active{background:#fff;color:#111}
 .sec{display:none;padding:0 12px}.sec.active{display:block}
 table{border-collapse:collapse;width:100%;font-size:12px}
 th{position:sticky;top:0;background:#333;color:#eee;padding:6px;text-align:left}
 th input{width:100%;box-sizing:border-box;margin-top:4px;font-size:11px}
 td{border-bottom:1px solid #eee;padding:6px 8px;vertical-align:top}
 .idx{color:#999}.ctr{text-align:center}.jar{font-family:monospace;color:#7a5c00}
 .desc{color:#999;font-family:monospace;font-size:10px}
 .stack{white-space:pre;font-family:monospace;font-size:11px;color:#555;border-left:3px solid #4a90d9;padding-left:8px}
</style></head><body>
<header>bindermap &mdash; AIDL &rarr; public API</header>
<div class="tabs">
 <a class="active" onclick="show('mapped',this)">Mapped ({{N_MAPPED}})</a>
 <a onclick="show('unmapped',this)">Unmapped ({{N_UNMAPPED}})</a>
</div>
<div id="mapped" class="sec active"><table><thead><tr>
 <th>#</th>
 <th>AIDL method<input data-col="aidl" oninput="filt()"></th>
 <th>Public API<input data-col="sdk" oninput="filt()"></th>
 <th>Hops<input data-col="hops" oninput="filt()"></th>
 <th>Jar<input data-col="jar" oninput="filt()"></th>
 <th>Call stack<input data-col="stack" oninput="filt()"></th>
</tr></thead><tbody>{{MAPPED_ROWS}}</tbody></table></div>
<div id="unmapped" class="sec"><table><thead><tr>
 <th>#</th>
 <th>AIDL method<input data-col="aidl" oninput="filt()"></th>
 <th>Reason<input data-col="reason" oninput="filt()"></th>
 <th>Deepest path<input data-col="stack" oninput="filt()"></th>
</tr></thead><tbody>{{UNMAPPED_ROWS}}</tbody></table></div>
<script>
 function show(id,el){document.querySelectorAll('.sec').forEach(s=>s.classList.remove('active'));
  document.querySelectorAll('.tabs a').forEach(a=>a.classList.remove('active'));
  document.getElementById(id).classList.add('active');el.classList.add('active');}
 function filt(){const s=document.querySelector('.sec.active');
  const f={};s.querySelectorAll('thead input').forEach(i=>{if(i.value)f[i.dataset.col]=i.value.toUpperCase();});
  s.querySelectorAll('tbody tr').forEach(r=>{let ok=true;
   for(const c in f){if(!(r.dataset[c]||'').toUpperCase().includes(f[c])){ok=false;break;}}
   r.style.display=ok?'':'none';});}
</script></body></html>"""


if __name__ == "__main__":
    render(sys.argv[1] if len(sys.argv) > 1 else ".")
