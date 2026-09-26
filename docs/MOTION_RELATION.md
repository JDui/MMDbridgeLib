# Motion and Camera Relations

For each VMD motion, candidate camera assets are searched in the same directory and no more than two child-directory levels below it. Pair scoring never moves or merges files.

## Name normalization

Compare Unicode stems normalized to NFC and lowercase after removing common channel words (`camera`, `cam`, `カメラ`, `motion`, `モーション`, `动作`, `动作数据`, `相机`, `camera_motion`), punctuation differences, and whitespace. This makes canonically equivalent NFC/NFD spellings compare consistently. Preserve version/fix tokens for version-family analysis rather than deleting them as identity evidence. Full Unicode case folding is not implemented; case variants outside Rust's lowercase mapping can remain reviewable proposals.

## Score inputs

- normalized filename / common prefix / common tokens / edit distance;
- explicit camera suffix or folder name;
- distance between directories (same directory is strongest; deeper than two levels is excluded);
- compatible asset types;
- version-token agreement or conflict.

The first implementation stores each component and a deterministic score from 0 to 1. A proposed `MotionCameraPair` is high confidence only when core names match and camera evidence is present. Lower-confidence links are kept as proposals with `NeedsReview`; no candidate is discarded because a different candidate scored higher.

Every relation stores source ID, target ID, relation type, score, reason codes, and confirmation state. Examples of reason codes include `normalized_filename_match`, `same_parent_directory`, `camera_suffix_detected`, `directory_distance_2`, and `version_token_conflict`.

## Current Core implementation

After a root scan, Core rebuilds unconfirmed `MotionCameraPair` and `VersionFamily` proposals from parsed Motion metadata. It searches the same directory and up to two child-directory levels for camera-bearing VMD assets. The deterministic score stores normalized-name equality, prefix similarity, common-token overlap, edit similarity, camera evidence, directory proximity, type compatibility, and version agreement in `reason_json`. Every eligible candidate above the minimum score is retained; the score is not used to pick a single winner.

Version proposals group same-core-name assets within nearby directories, preserve detected suffix labels such as `v2`, `fix`, and `修正版`, and include an unversioned member as `Original`. A family is written to the `versions` index and its pairwise suggestions are exposed as `VersionFamily` relations. The desktop inspector and `mmdbridge relations list` show proposals; `mmdbridge relations confirm <id>` or the inspector can confirm one. A later refresh replaces unconfirmed automatic suggestions while retaining confirmed relations. This initial resolver uses parsed metadata, names, and directories; it does not compare motion curves or model geometry.

## Version families

Version tokens such as `v2`, `fix`, `修正版`, and numbered suffixes produce a `VersionFamily` proposal with explicit member paths and preserved ordering evidence. Original and revised motions remain separate assets. No automatic rename, overwrite, merge or deletion is permitted.
