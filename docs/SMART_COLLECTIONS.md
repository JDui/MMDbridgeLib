# Smart Collections

Smart Collections are saved, versioned JSON filter expressions evaluated by Core. The desktop UI builds them and the CLI can list, save, run, update, and remove them. A filter never modifies source assets.

## Expression

Groups use `and` or `or`, `not` wraps one expression, and a rule contains a field, operator, and typed value:

```json
{
  "op": "and",
  "children": [
    { "op": "rule", "field": "assetType", "operator": "eq", "value": "motion" },
    { "op": "not", "child": { "op": "rule", "field": "hasCamera", "operator": "eq", "value": true } }
  ]
}
```

Core validates every field/operator/value and compiles only whitelisted SQL expressions with bound parameters. A filter is limited to 64 nodes, eight nested groups, and 32 children per AND/OR group. `contains` treats `%`, `_`, and backslash as literal text.

## Fields

| Field | Value | Operators |
| --- | --- | --- |
| `assetType`, `rootId`, `directory`, `tag`, `cardStatus`, `fileType` | Text | `eq`, `ne`; `contains` where applicable |
| `favorite`, `relationStatus`, `needsReview`, `hasThumbnail`, `hasCard` | Boolean | `eq`, `ne` |
| `polygonCount`, `boneCount`, `frameCount`, `duration`, `width`, `depth`, `area` | Number | `eq`, `ne`, `gt`, `gte`, `lt`, `lte` |
| `recentlyAdded`, `recentlyModified` | RFC3339 timestamp | `eq`, `ne`, `gt`, `gte`, `lt`, `lte` |
| `hasBoneMotion`, `hasMorphMotion`, `hasCamera`, `pose`, `hasPairedCamera` | Boolean | `eq`, `ne` |

Numeric and type-specific values come from parsed metadata. `hasPairedCamera` checks the Motion/Camera relation index; `relationStatus` checks any stored relation. Card presence and thumbnail presence are distinct.

The legacy `cameraOnly` field remains readable so saved filters can be recognized and disabled with an edit notice. Core rejects it, and normal asset queries filter auxiliary pure-Camera VMDs before pagination so a saved condition cannot expose them.

Legacy collections containing `duplicateStatus` or a `fileType` rule for X retain their original name and expression. The desktop marks them “条件已停用，需要编辑” and will not apply them; Core also rejects these retired conditions, including when nested in groups. Editing a saved collection in place is not currently available, so create a replacement collection without those conditions.

The persistent `saved_filters` table stores the name and expression JSON. Applying a saved filter can also take the Library's free-text search term; the result uses the same Core asset response and whitespace-separated AND token matching as ordinary browsing. Desktop collection results use a virtualized grid and request 500-row keyset-cursor pages. Core caps each page at 50,000 rows for non-desktop callers. The UI requests more pages as the user reaches the end of the current results, so collections can exceed one page without loading the full result set into memory.

## CLI

```powershell
mmdbridge filters list --json
mmdbridge filters save --name "Camera-free motions" --expression '{"op":"and","children":[{"op":"rule","field":"assetType","operator":"eq","value":"motion"},{"op":"rule","field":"hasCamera","operator":"eq","value":false}]}' --json
mmdbridge filters run <filter-id> --json
mmdbridge filters remove <filter-id> --json
```
