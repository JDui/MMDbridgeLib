# MMDbridgeLib Architecture

## Boundary

`mmdbridge-core` owns asset discovery, parsers, fingerprints, SQLite access, card files, thumbnail jobs and relations. The Tauri application and `mmdbridge` CLI are adapters over that Rust API. The React frontend only renders returned data and sends user actions through Tauri commands.

The filesystem and valid nearby `.MMDRCV` files are the durable sources. SQLite is a rebuildable index and job/cache store. Removing a root from the library never changes its files.

The desktop app bundles an interactive PMX diagnostic viewer. Core parses PMX and returns a versioned binary geometry/weight payload; the frontend only displays that parsed data with the locally bundled Three.js renderer. This on-demand inspector is separate from the native thumbnail renderer and is not used to generate cards.

The desktop-only `asset_reveal` and `asset_open_directory` adapters select the indexed primary source or open its source directory in Windows Explorer. They pass Unicode paths as process arguments and do not modify asset files.

Library collections use keyset pagination ordered by case-insensitive asset name and asset ID. The cursor carries the last returned name/ID pair; the UI requests another bounded page when the virtualized grid reaches its end. Root, search, favorite, saved-filter, and duplicate-membership conditions are applied in Core before paging.

```text
React UI ── Tauri commands ──┐
                             ├── mmdbridge-core ── SQLite index
CLI / Agent Skill ──────────┘          ├── filesystem and parsers
                                       ├── thumbnail job pool and stage gates
                                       └── MMDRCV reader/writer
```

## Packages

- `crates/mmdbridge-core`: public Rust API and all asset rules.
- `crates/mmdbridge-cli`: structured command-line adapter; no duplicate parsing or database rules.
- `apps/desktop`: React, TypeScript and Vite UI.
- `apps/desktop/src-tauri`: Tauri v2 shell and command adapters.
- `skills/mmdbridge-card-manager`: generic instructions for using the CLI.

## Storage and identity

- The portable desktop app and CLI share `data/library.sqlite3` beside their EXE files. WAL mode, foreign keys, schema migrations, a 512 MiB database cap and a 32 MiB journal size target are enabled. The Settings panel shows storage use and offers idle-time compaction.
- Root paths remain native filesystem paths in Rust and are encoded as Unicode strings in JSON/SQLite. On Windows, directory walking and file I/O must preserve Unicode and long-path prefixes.
- Every source file is represented in `asset_files`; a candidate `assets` row points at its primary source and package directory.
- PMX, PMD, and text/binary-X parsing records material texture references. Existing files contained by the model's package directory are indexed as `asset_files` dependencies; missing and external references remain visible in parsed metadata, and dependency file changes trigger an incremental reparse.
- Asset IDs are UUIDs created once. An unchanged primary-source fingerprint can reconnect a moved file to the existing ID. A changed source at the same path retains its ID and marks its card stale. Ambiguous matches stay separate and gain `NeedsReview`.
- BLAKE3 is used for source fingerprints and exact duplicate grouping. Size and modified time are a fast unchanged-file hint; they are not a substitute for a content fingerprint when a source changes or moves. This is not a SHA checksum or a security/signature check.
- MMDRCV stays beside its asset. Deleting SQLite and rescanning must rebuild the index without altering source files.

## Parser dependency

The core wraps the MIT-licensed `mmd-anim-format` parser behind its own `AssetMetadata` API. PMX, PMD, VMD, VPD and text-X use its structured parsers; uncompressed binary X uses a Core token decoder that adapts standard mesh objects into the same accessory manifest. Format decoding remains in Rust Core and is not reimplemented in the UI. The dependency is pinned to the reviewed minor release while the core adapter limits API churn. Parser limitations and diagnostics become structured asset status rather than silent partial success.

The parser does not provide a finished MMD thumbnail renderer. Core therefore owns a separate native Rust renderer using `wgpu`; it renders PMX, PMD, text and uncompressed binary X, and configured-model VMD/VPD previews without WebView screenshots. Renderer coverage, diagnostics and real-asset smoke evidence are tracked in `docs/THUMBNAIL_RENDERER.md`.

## Initial SQLite tables

The schema is versioned and includes `roots`, `assets`, `asset_files`, `metadata`, `tags`, `asset_tags`, `asset_tag_overrides`, `favorites`, `relations`, `duplicates`, `versions`, `cards`, `jobs`, `operation_journal`, `scan_state`, `settings`, and `saved_filters`. Foreign keys and indexes are owned by Core schema updates. API responses use serde structures; callers never issue SQL.

## Delivery stages

1. Specification and package boundaries: established in `docs/` and `skills/`.
2. Core library: typed roots, PMX/VMD/VPD/PMD/X metadata, incremental index, SQLite API and CLI are implemented.
3. Tauri library UI: browse, search, saved Smart Collections, inspector, roots, local PMX preview/weight viewer and safe library state are implemented.
4. Native thumbnails, MMDRCV validation/writer and the persistent cacheable queue are implemented for the formats and render modes documented in `docs/THUMBNAIL_RENDERER.md`.
5. Agent CLI/Skill flow is implemented over the same Core API.
6. Initial Motion-camera and version-family proposals plus exact and scored possible-duplicate suggestions are connected to scan/Core/CLI/Tauri/UI; deeper motion-curve matching remains future work.
7. Package-aware move, rename and Recycle Bin deletion now use a plan-confirm-execute flow with dependency/root/job safety checks and a persistent recovery journal. Automatic undo is not implemented; unsupported loose-root assets and external dependencies are blocked for safety.

Renderer output on broader model groups and end-to-end desktop watcher/UI refresh behavior still need validation. A standalone Windows `notify` recursive-watcher smoke observed a `.x` file created in a nested Unicode directory, and an in-memory Core scan indexed it without starting Tauri. A real binary `.x` scene has rendered headlessly on the RTX 4090/Vulkan adapter; disposable Windows smokes have checked package rename, cross-volume move and silent Recycle Bin execution. A disposable copy of a real scene package moved 15 indexed assets and 25 dependencies with the index and journal updated. More complex packages remain unverified.
