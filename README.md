# bindermap

Reverse-trace AIDL/binder interface methods to the public SDK methods that reach
them, from DEX bytecode.

Given a set of framework DEX artifacts and a public-API oracle, bindermap discovers
every AIDL interface method (an `IInterface` subtype's methods) and traces backwards
through the reverse call graph to find which public SDK methods ultimately invoke it.
The output is a mapping table (CSV/JSON); rendering to HTML is a separate step.

## What it computes

An AIDL method like `IActivityManager.getRunningAppProcesses` is a binder
transaction — a client reaches it indirectly, through a public SDK call such as
`ActivityManager.getRunningAppProcesses`. bindermap recovers that link by inverting
the call graph and searching outward from each AIDL method until it reaches a method
that is part of the public SDK, recording the shortest such chain (and, with
`--all-paths`, every one).

Example (real, from `framework.jar`):

```
android.app.IActivityManager.addInstrumentationResults
    -> android.app.Instrumentation.addResults        (1 hop)
```

## How it works

The analysis is bytecode-level; virtual dispatch is resolved through the class
hierarchy, not guessed from names.

1. **Decode** (`dex`). `dexdump -d` output is parsed into a class graph: each class's
   name, superclass, interfaces, access flags, and per-method invoke targets. Only
   the hierarchy and call edges are modeled. `classes*.dex` are extracted from
   jars/apks in-process; inputs are decoded in parallel.
2. **Oracle** (`api`). The set of public-API method signatures (`owner/name+desc`),
   loaded from either a signature list or an SDK dex/jar (whose every method is taken
   as public).
3. **Index** (`graph`). Built once, in parallel: the reverse call graph
   (callee → callers) from the invoke edges; each class's transitive ancestors; the
   AIDL entrypoints (interfaces transitively extending `android.os.IInterface`,
   excluding `<init>`/`asBinder`); and, for every concrete method, the public-API
   method it overrides or implements.
4. **Trace** (`trace`). A reverse breadth-first search from each AIDL method through
   the reverse graph. A method is a hit if it is itself in the public SDK, or if it
   overrides a public-API method. Server-side `$Stub` dispatch is skipped (its
   `onTransact` traces back through the binder runtime into generic hits); the client
   `$Stub$Proxy` is not. Inner classes bridge to their enclosing class. A desugared
   lambda / method reference — a synthetic class invoked through a functional
   interface, with no direct callers — bridges to its construction site (via the
   `ACC_SYNTHETIC` flag, not a name), so a closure that reaches binder is attributed
   to the method that created it. Search is bounded to 50 hops. Fast mode stops each
   branch at the first public hit; `--all-paths` records every one.
5. **Report** (`report`). Serializes the deduped, sorted result to CSV and JSON. It
   holds no analysis state, so rendering stays decoupled.

Because D8 desugars lambdas into synthetic classes, lambda calls are ordinary invokes
in DEX — there is no `invokedynamic` special case to handle.

## Usage

```sh
# oracle from a dexed SDK (d8 android.jar once), traced over framework jars
bindermap --public-api sdk.dex \
  $OUT/system/framework/framework.jar \
  $OUT/system/framework/services.jar \
  --out-dir ./out

# oracle from a signature list instead (one `owner/name+desc` per line)
bindermap --public-api sdk_methods.txt framework.jar --out-dir ./out

# comprehensive (every public entrypoint per chain, slower)
bindermap --all-paths --public-api sdk.dex framework.jar --out-dir ./out
```

Inputs are `.dex` / `.jar` / `.apk` files or directories (scanned recursively).
`dexdump` is required to decode DEX; point `$BINDERMAP_DEXDUMP` at it or have it on
`PATH`. The public SDK `android.jar` is class files, not DEX — convert it once with
`d8 --output sdk/ android.jar` and pass `sdk/` (or `sdk/classes.dex`).

## Output

- `binder_mapping.csv` — `BINDER_INTERFACE_METHOD, PUBLIC_API_CLASS, METHOD, DESC, HOPS, JAR`.
- `unmapped_aidl.csv` — AIDL methods with no public path (`NO_PUBLIC_PATH` /
  `NO_CALLERS_FOUND`).
- `trace_debug.log` — the call chain per AIDL method.
- `binder_mapping.json` — the same result, structured.

`--all-paths` runs write the `*_all` variants.

## Rendering

`tools/report.py` reads the CSVs and `trace_debug.log` and writes a single
filterable `report.html`. It is a pure consumer of the output — no shared state with
the analyzer, and no links beyond the public developer reference.

```sh
python3 tools/report.py ./out    # writes ./out/report.html
```

## Build and test

```sh
cargo build --release
cargo test            # unit tests are self-contained
```

The end-to-end test traces a prebuilt DEX fixture and runs only when `dexdump` is
available (skipped otherwise, so `cargo test` needs no toolchain).

Verified on AOSP: over `framework.jar` + `services.jar` with a d8'd public
`android.jar` oracle, it maps ~10k AIDL→public-API links in ~15s, with no leaks into
`com.android.internal` or the `Binder` runtime.
