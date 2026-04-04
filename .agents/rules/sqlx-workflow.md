---
trigger: always_on
---

## SQLx Workflow
	
### Overview
	
This project uses `sqlx` with offline mode (`sqlx-data.json` / `.sqlx/` query cache). During development there is **no production data or backward compatibility concern**, so the database is treated as fully disposable.
	
### Schema Change Pipeline
	
Whenever the database schema changes (new/modified migrations, column changes, table changes), run the full reset pipeline **in order**:
	
```bash
sqlx database drop -f -y
sqlx database create
sqlx migration run
cargo sqlx prepare --workspace
```

No exceptions. Do not attempt incremental migration fixes or manual ALTER statements. Blow it away and rebuild.

When to Run the Pipeline

- Adding, editing, or deleting any file in the migrations/ directory

- Changing any sqlx::query! / sqlx::query_as! macro invocation (schema-dependent)

- After pulling changes that touch migrations or query cache files

- When cargo build fails with sqlx offline-mode cache misses

Migration Authorship

1. Create migrations via the CLI:

sqlx migrate add <descriptive_name>

2. Write only forward (up) migrations. Reversible migrations are unnecessary — we drop and recreate.

3. Keep each migration focused on a single logical change for readable git history.

Offline Mode / CI

- The .sqlx/ directory (or sqlx-data.json) MUST be committed to the repo so CI can build without a live database.

- After running cargo sqlx prepare --workspace, always verify the updated cache files are staged before committing:

git add .sqlx/


Environment

- Ensure DATABASE_URL is set (via .env or environment variable) before running any sqlx command.

- The sqlx-cli tool should be installed by default, if not:

cargo install sqlx-cli

Checklist Before Committing Schema Changes

-  Full pipeline executed: drop → create → migrate → prepare

-  .sqlx/ query cache is up to date and staged

-  cargo build succeeds in offline mode (unset DATABASE_URL and verify)

-  No leftover manual DDL outside of migration files