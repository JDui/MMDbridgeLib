# Asset Discovery

## Roots

Each root has a stable UUID, `asset_type` (`model`, `motion`, or `scene`), absolute path, display name, enabled flag, recursion flag, creation time, last scan time and scan status. A root is a filter/scope, not ownership of its files. Removing it only removes the index configuration.

The desktop app watches enabled roots using the root's recursion setting. Filesystem changes are coalesced for 900 ms, then sent through the same Core scan path used by manual scans. `.MMDRCV` sidecar writes are ignored to avoid rescans caused by card publication. The desktop polls persisted scan state and refreshes the library when a scan completes. The CLI remains explicit and does not start a watcher.

Desktop manual and watcher scans enter a single Core scan queue. `scan_state` persists Pending, Discovering, Indexing, Verifying, Relations, Duplicates, Pausing, Paused, Cancelling, Completed, Failed, and Cancelled states with a percentage, file counts, and queue order. The separate desktop scan queue supports pause, continue, stop, retry, and moving pending scans up or down. Pause and stop keep indexed assets and skip the incomplete scan's final missing-source and relation work; continuing rechecks cached unchanged files. Pending and interrupted entries become Paused when the desktop next starts, so closing the window does not silently restart a scan. The CLI `scan --root` remains a synchronous Core operation and updates the same progress state.

The scanner reports progress and errors as structured records. It uses native `PathBuf` operations, preserves Unicode, does not follow directory symlinks by default, and checks the stored size/mtime before re-reading metadata. Previously indexed `Unsupported` `.x` files get a header-only check so uncompressed binary X can be reparsed after this parser upgrade without rescanning every unchanged asset. A full-content BLAKE3 fingerprint is refreshed when a file changes or must be re-associated after a move.

On 2026-09-25, a Windows Core scan discovered and parsed a VPD at a 373-character absolute path with `Ready` status and zero parse failures. This validates the Core scanner/parser path; desktop watching and file-picker behavior at that path depth remain unverified.

Also on 2026-09-25, a standalone Windows `notify` recursive watcher observed a new `.x` file copied into a nested Chinese/Japanese directory. After the event and the production 900 ms quiet period, an in-memory Core Library scan indexed the copied file. The disposable directory was removed. This verifies native watcher → Core scan behavior without starting Tauri; Tauri event delivery, desktop library refresh, and file-picker/long-path behavior remain unverified.

A synthetic VMD scan also confirmed the pose boundary: one bone frame plus one static camera key is classified as a pose, while camera keys at two frames are classified as animation.

## Search

Core text search matches the asset name, primary filename/path, package directory, and attached tag names. Whitespace-separated terms use AND matching: every term must occur in at least one searchable field, and different terms may match different fields. The same search is used by paginated Library results, duplicate results, saved collections, and favorites. Query terms try both NFC and NFD spellings so canonically equivalent Chinese, Japanese, and Latin text can match; `%`, `_`, and `\\` are treated literally. Fuzzy ranking is not implemented.

A Core smoke using a Chinese/Japanese tag matched through the list, paginated, and favorites APIs.

## Candidate rules

| Root type | Primary formats | First parser |
| --- | --- | --- |
| Model | PMX | PMX |
| Motion | VMD, VPD | VMD animation, VPD pose |
| Scene | PMX, PMD, text/binary X | PMX, PMD, X |

A PMX belongs to the root's declared type. This avoids guessing whether an arbitrary PMX under a model root is a model or a stage. Scene PMX and model PMX use the same parser but produce type-specific metadata.

The initial scanner emits one candidate per primary file. A lone PMX in a directory is an ordinary candidate. Multiple PMX files in one directory are separate candidates marked `NeedsReview`; the scanner does not merge variants based on names or structural similarity. Package dependency resolution and candidate grouping can later propose a user-confirmed directory package.

## Metadata

- PMX model: internal name (fallback to filename), vertex count, polygon count (`index_count / 3`), material/bone/morph/physics counts, version and parser diagnostics.
- Motion: keyframe range, inclusive frame count, seconds at 30 FPS, bone/morph/camera/light/property channel flags, camera-only and pose classification.
- VPD is always a pose. A VMD is pose-like only when all bone/morph keys occupy one frame and no camera, light, IK/property, or self-shadow channel changes across frames. A single static camera/light key does not disqualify a pose; the rule is retained in metadata so the user can correct an ambiguous result.
- Scene: source type, polygon count and XZ bounds in original MMD units. Area is `width * depth`; it is not converted to meters.
- Text X and uncompressed binary X provide mesh vertices and faces for bounds. Binary X supports the standard Mesh, MeshNormals, MeshTextureCoords, MeshVertexColors, MeshMaterialList, Material and TextureFilename objects; compressed X encodings remain `Unsupported`.

## Identity, changes and status

The source path, file size, modified time, BLAKE3 content fingerprint and parser version are cached. Same path plus changed bytes preserves the current asset ID and invalidates metadata/card state. A moved source with a matching fingerprint may retain its ID if there is exactly one candidate. Multiple matches remain separate and are flagged as possible duplicates.

Statuses are additive: `Ready`, `NeedsReview`, `MissingSource`, `ParseFailed`, `Unsupported`, `CardMissing`, `CardStale`, and `CardBroken`. Parser failures include an error code, message, source, asset ID when known, and recoverability.

No scanner path renames, moves, merges or deletes user files.

## Duplicate suggestions

Exact duplicate pairs are grouped by equal BLAKE3 fingerprints. Possible-duplicate suggestions are scored from normalized asset names, primary-file size, available parsed polygon/bone or motion frame/duration counts, and relative directory structure. Equal fingerprints stay in the exact category; differing cryptographic fingerprints are not treated as a meaningful distance metric. The candidate pass compares nearby normalized names, small same-directory groups, and matching structural groups to avoid an all-pairs scan. Its score is a review hint, not proof that two assets are interchangeable.

Both categories are stored as suggestions with component evidence. They never trigger an automatic delete, overwrite, move, or merge.

Tags store their origin on each asset assignment. The desktop supports multi-select batch assignment/removal of user tags and favorite changes through atomic Core transactions. A failed batch rolls back as a whole; tag responses include a mutation result for each selected asset, while favorite responses report the number whose state changed. Manual user tags take priority over Agent or Parser proposals; removing a tag records a per-asset override on every selected asset so later automatic proposals do not put it back. Favorites are stored separately from tags.
