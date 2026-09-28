# Asset Discovery

## Roots

Each root has a stable UUID, `asset_type` (`model`, `motion`, or `scene`), absolute path, display name, enabled flag, recursion flag, creation time, last scan time and scan status. A root is a filter/scope, not ownership of its files. Removing it only removes the index configuration.

The desktop app watches enabled roots using the root's recursion setting. Filesystem changes are coalesced for 900 ms, then sent through the same Core scan path used by manual scans. `.MMDRCV` sidecar writes are ignored to avoid rescans caused by card publication. The desktop polls persisted scan state and refreshes the library when a scan completes. The CLI remains explicit and does not start a watcher.

The desktop has a persistent Folder entry beside Library navigation. It can open the saved root/directory, browse configured roots without selecting a type first, and navigate immediate child directories returned by Core. `directory_page` validates the requested path against its root, moves to the nearest existing parent when a folder disappears, and reports the visible count and direct children from `asset_directory_counts`. The folder view's Include subdirectories switch is applied by Core before paginated listing. Viewing an asset's folder clears hidden search/favorite/collection conditions; returning to Assets restores the prior filter and scroll offset. The folder root, path, recursion scope, and view mode are stored in local application preferences.

Desktop manual and watcher scans enter a single Core scan queue. `scan_state` persists Pending, Discovering, Indexing, Verifying, Relations, Pausing, Paused, Cancelling, Completed, Failed, and Cancelled states with a percentage, file counts, and queue order. The separate desktop scan queue supports pause, continue, stop, retry, and moving pending scans up or down. Pause and stop keep indexed assets and skip the incomplete scan's final missing-source and relation work; continuing rechecks cached unchanged files. Pending and interrupted entries become Paused when the desktop next starts, so closing the window does not silently restart a scan. The CLI `scan --root` remains a synchronous Core operation and updates the same progress state.

The scanner reports progress and errors as structured records. It uses native `PathBuf` operations, preserves Unicode, does not follow directory symlinks by default, and checks the stored size/mtime before re-reading metadata. Paths are normalized once for scan ordering, and dependency file stamps are cached by normalized path for the duration of a scan. Each parsed asset stores its format-specific parser revision; only assets with an obsolete revision are reparsed when parser rules change. Unchanged rows avoid routine writes, and status changes are written only when the status actually differs. Schema v11 marks previously indexed `.x` scenes as retired; they remain in historical storage but are excluded from discovery, active results and cached counts. Schema v12 adds `assets.visibility` and moves strictly pure-Camera VMDs to `auxiliary`: a file must contain Camera keys and zero Bone, Morph, Light, Property, and Self Shadow keys. Old metadata is migrated only when all six key counts are present and parse-success metadata is intact; an unchanged VMD without the current classification marker is reparsed on its next scan. Schema v13 stores MMDRCV file size/mtime and manifest/renderer revisions so a normal scan can skip deep card reads while the card remains unchanged; the first scan after migration fills these cache fields. Paginated desktop results use a compact `AssetListItem` and fetch full parsed metadata only for the selected inspector item. Ordinary lists, favorites, filters, cards, new/retried thumbnail jobs, default card batches, and cached counts include only normal assets. Hidden Camera rows, tags, favorites, existing card files, and historical job records remain stored; motion relations and preview can still use the source VMDs. New `.x` files are ignored as unsupported formats. A full-content BLAKE3 fingerprint is refreshed when a supported file changes or must be re-associated after a move.

On 2026-09-25, a Windows Core scan discovered and parsed a VPD at a 373-character absolute path with `Ready` status and zero parse failures. This validates the Core scanner/parser path; desktop watching and file-picker behavior at that path depth remain unverified.

Before G3-04 retired X support, a standalone Windows `notify` recursive watcher observed a new `.x` file copied into a nested Chinese/Japanese directory. After the event and the production 900 ms quiet period, an in-memory Core Library scan indexed the copied file. The disposable directory was removed. This historical smoke did not start Tauri; desktop watcher delivery, library refresh, and file-picker/long-path behavior remain unverified.

A synthetic VMD scan also confirmed the pose boundary: one bone frame plus one static camera key is classified as a pose, while camera keys at two frames are classified as animation.

## Search

Core text search matches the asset name, primary filename/path, package directory, and attached tag names. Whitespace-separated terms use AND matching: every term must occur in at least one searchable field, and different terms may match different fields. The same search is used by paginated Library results, saved collections, and favorites. Query terms try both NFC and NFD spellings so canonically equivalent Chinese, Japanese, and Latin text can match; `%`, `_`, and `\\` are treated literally. Fuzzy ranking is not implemented.

A Core smoke using a Chinese/Japanese tag matched through the list, paginated, and favorites APIs.

## Candidate rules

| Root type | Primary formats | First parser |
| --- | --- | --- |
| Model | PMX | PMX |
| Motion | VMD, VPD | VMD animation, VPD pose |
| Scene | PMX, PMD | PMX, PMD |

A PMX belongs to the root's declared type. This avoids guessing whether an arbitrary PMX under a model root is a model or a stage. Scene PMX and model PMX use the same parser but produce type-specific metadata.

The initial scanner emits one candidate per primary file. A lone PMX in a directory is an ordinary candidate. Multiple PMX files in one directory are separate candidates marked `NeedsReview`; the scanner does not merge variants based on names or structural similarity. Package dependency resolution and candidate grouping can later propose a user-confirmed directory package.

## Metadata

- PMX model: internal name (fallback to filename), vertex count, polygon count (`index_count / 3`), material/bone/morph/physics counts, version and parser diagnostics.
- Motion: keyframe range, inclusive frame count, seconds at 30 FPS, bone/morph/camera/light/property channel flags, camera-only and pose classification.
- VPD is always a pose. A VMD is pose-like only when all bone/morph keys occupy one frame and no camera, light, IK/property, or self-shadow channel changes across frames. A single static camera/light key does not disqualify a pose; the rule is retained in metadata so the user can correct an ambiguous result.
- Pure-Camera VMDs are auxiliary rather than top-level motion assets. Camera plus any other VMD channel remains visible, and relation records let a motion select and preview an auxiliary paired Camera from the same or supported child folders.
- Scene: source type, polygon count and XZ bounds in original MMD units. Area is `width * depth`; it is not converted to meters.
- Previously indexed X scenes are retained as retired historical records and are not parsed or rendered. Their source files and sidecars are not changed by retirement.
- Requests that inspect or modify a retired asset by ID return the retired-format error; historical tag reads remain available, while tag and favorite changes are blocked.

## Identity, changes and status

The source path, file size, modified time, BLAKE3 content fingerprint and parser version are cached. Same path plus changed bytes preserves the current asset ID and invalidates metadata/card state. A moved source with a matching fingerprint may retain its ID if there is exactly one candidate. Multiple matches remain separate; ambiguous identity associations are marked `NeedsReview`.

Statuses are additive: `Ready`, `NeedsReview`, `MissingSource`, `ParseFailed`, `Unsupported`, `CardMissing`, `CardStale`, and `CardBroken`. Parser failures include an error code, message, source, asset ID when known, and recoverability.

No scanner path renames, moves, merges or deletes user files.

## Duplicate management retirement

Duplicate suggestions and their management UI have been removed. BLAKE3 remains an internal source fingerprint for reconnecting a moved file to an existing asset ID; it no longer groups or labels assets as duplicates. Existing assets, IDs, and source files are preserved.

Tags store their origin on each asset assignment. The desktop supports multi-select batch assignment/removal of user tags and favorite changes through atomic Core transactions. A failed batch rolls back as a whole; tag responses include a mutation result for each selected asset, while favorite responses report the number whose state changed. Manual user tags take priority over Agent or Parser proposals; removing a tag records a per-asset override on every selected asset so later automatic proposals do not put it back. Favorites are stored separately from tags.
