---
trigger: always_on
---

## Rust Workspace Dependency Management

### Workspace Structure

This is a **Cargo workspace project** where crates are split per feature/component. All Rust code lives under a workspace defined in the root `Cargo.toml`.

---

### Root `Cargo.toml` Rules

1. **`resolver = "2"` MUST be set** in `[workspace]` to prevent feature unification across workspace members.
2. All external dependencies MUST be declared in `[workspace.dependencies]`.
3. Every workspace dependency MUST specify `default-features = false`. Feature selection is the sole responsibility of each subcrate.
4. Do NOT declare `optional` on workspace-level dependency entries — optionality is a per-crate concern.

```toml
[workspace]
resolver = "2"
members = [
    "crates/core",
    "crates/http",
    "crates/db",
    "crates/my-lib",   # aggregator façade
]

[workspace.dependencies]
serde  = { version = "1",   default-features = false }
tokio  = { version = "1",   default-features = false }
axum   = { version = "0.8", default-features = false }
sqlx   = { version = "0.8", default-features = false }
```

---

### Subcrate `Cargo.toml` Rules

1. Reference all external deps via `workspace = true`.
2. Declare **only the features that subcrate actually needs**.
3. Do NOT repeat `version` or `default-features` — those come from the workspace root.
4. Mark internal path-deps as `optional = true` when they are conditionally needed.

```toml
# crates/http/Cargo.toml
[dependencies]
tokio = { workspace = true, features = ["net", "rt-multi-thread"] }
axum  = { workspace = true, features = ["http1", "tokio"] }
serde = { workspace = true, features = ["derive"] }
```

---

### Aggregator / Façade Crate Rules

When a top-level crate re-exports subcrates as public features:

1. Declare internal subcrate path-deps as `optional = true`.
2. Gate them behind named `[features]` using the `dep:` prefix to prevent the dep name from leaking as an implicit feature.
3. Provide a `full` feature that activates all optional crates.

```toml
# crates/my-lib/Cargo.toml
[dependencies]
core = { path = "../core" }                          # always required
http = { path = "../http", optional = true }
db   = { path = "../db",   optional = true }

[features]
http = ["dep:http"]
db   = ["dep:db"]
full = ["http", "db"]
```

> **Why `dep:` prefix?** Without it, declaring `http = { ..., optional = true }` implicitly creates a feature also named `http`, which can be activated unintentionally. `dep:http` cleanly separates the dependency from the feature name.

---

### Adding New Dependencies

1. Resolve the latest version from the workspace root — never guess:

```bash
# Dry-run to read the resolved version
cargo add <crate_name> -p <subcrate> --dry-run

# Or add then promote to workspace
cargo add <crate_name> -p <subcrate>
```

2. Move the entry into `[workspace.dependencies]` with `default-features = false`, remove `version` from the subcrate entry, and add `workspace = true`.

3. In the subcrate, keep only the `features = [...]` it needs.

---

### When APIs Break or Are Unclear

1. Before using any dep's API, verify against the exact pinned version:
   - `https://docs.rs/<crate_name>/<version>`
   - `cargo doc --open -p <crate_name>`

2. If a compile error suggests an API change, check the crate's changelog before modifying code.

3. Do NOT assume API signatures from memory — always verify against the pinned version's docs.

---

### Auditing Feature Unification

Run these after adding or changing deps to verify features are not bleeding across crates:

```bash
# Full feature tree for a specific crate
cargo tree -e features -p <crate_name>

# Check which features land on a specific dep
cargo tree -e features -i tokio

# Detect unintended duplicate compilations
cargo tree --duplicates
```

With `resolver = "2"`, the same dep may legitimately compile twice with *different* feature sets (e.g., once for a build-script, once for the lib). Use `--duplicates` to distinguish intentional from unintentional duplication.

---

### Checklist Before Committing

- [ ] `resolver = "2"` is present in `[workspace]`
- [ ] New external dep is in `[workspace.dependencies]` with `default-features = false`
- [ ] Subcrate entry uses `workspace = true` with no `version` or `default-features`
- [ ] Subcrate specifies only the features it needs via `features = [...]`
- [ ] Optional subcrate deps in the aggregator use `dep:` prefix in `[features]`
- [ ] Version was resolved via `cargo add` / `cargo search`, not guessed
- [ ] API usage verified against docs for the pinned version
- [ ] `cargo tree --duplicates` shows no unexpected duplicate compilations
