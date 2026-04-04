---
trigger: always_on
---

## Rust Workspace Dependency Management
	
### Workspace Structure
	
This is a **Cargo workspace project**. All Rust code lives under a workspace defined in the root `Cargo.toml`.
	
### Root `Cargo.toml` Rules
	
1. All external dependencies MUST be declared in `[workspace.dependencies]` in the root `Cargo.toml`.
2. Every dependency MUST specify `default-features = false`.
3. Do NOT specify features at the workspace level — feature selection is the responsibility of each subcrate.
	
Example root declaration:
	
```toml
[workspace.dependencies]
serde = { version = "1", default-features = false }
tokio = { version = "1", default-features = false }
```

Subcrate Cargo.toml Rules

1. Subcrates MUST reference dependencies via workspace = true.

2. Subcrates MUST declare only the features they actually need.

3. Do NOT specify version or default-features in subcrate dependency entries — those come from the workspace root.

Example subcrate declaration:

```toml
[dependencies]
serde = { workspace = true, features = ["derive"] }
tokio = { workspace = true, features = ["rt", "macros"] }
```

Adding New Dependencies

1. Always use cargo add first to resolve the latest version:

# From the workspace root:
cargo add <crate_name> --dry-run


Use the resolved version to manually add the entry to [workspace.dependencies] with default-features = false.

2. 
Alternatively, add directly then edit:

cargo add <crate_name>

Then move the entry into [workspace.dependencies], set default-features = false, and update the subcrate to use workspace = true with its required features.

3. Never hardcode or guess versions. Always let cargo add or cargo search resolve the latest.

When APIs Break or Are Unclear

1. Before using any dependency's API, consult the docs for the exact version declared in the workspace root:
- Use https://docs.rs/<crate_name>/<version> or

- Run cargo doc --open -p <crate_name>

- Consult pulled source code

2. If a compile error suggests an API change, check the crate's changelog/migration guide before modifying code.

3. Do NOT assume API signatures from memory — verify against docs of the pinned version.

Checklist Before Committing

-  New dependency exists in root [workspace.dependencies] with default-features = false

-  No version or default-features key in any subcrate Cargo.toml for workspace deps

-  Subcrate specifies only the features it needs via features = [...]

-  Version was resolved via cargo add / cargo search, not guessed

-  API usage matches docs for the declared version