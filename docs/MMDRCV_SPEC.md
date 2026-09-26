# MMDRCV Resource Card Specification

## Container

`.MMDRCV` is a ZIP-compatible container containing only the card payloads:

```text
manifest.json
preview.webp   # optional until a thumbnail exists
```

It must not contain PMX, VMD, textures, or a copy of the source package. Writers create a sibling temporary file, close and validate it, then publish it atomically. Existing cards are never overwritten when ownership cannot be established.

## Manifest schema version 1

```json
{
  "format": "MMDRCV",
  "schema_version": 1,
  "asset_id": "",
  "asset_type": "model",
  "name": "",
  "source": {
    "primary_file": "",
    "relative_path": "",
    "fingerprint": "blake3:<hex>"
  },
  "metadata": {},
  "tags": [],
  "suppressed_tags": [],
  "favorite": false,
  "thumbnail": null,
  "created_at": "",
  "updated_at": "",
  "generator": {
    "name": "MMDbridgeLib",
    "version": ""
  }
}
```

When a preview exists, `thumbnail` contains:

```json
{
  "file": "preview.webp",
  "width": 1024,
  "height": 1024,
  "format": "webp",
  "quality": 50
}
```

Readers support schema version 1 and ignore unknown manifest fields. `asset_type` is `model`, `motion`, or `scene`. The algorithm identifier is part of the fingerprint value so later algorithms can coexist. `suppressed_tags` preserves explicit user removals and `favorite` preserves the favorite flag across index rebuilds; both are optional MMDbridgeLib extensions for older cards.

## Filename and location

Cards live beside their primary asset. The visible name remains in the manifest; the filesystem name is sanitized separately. When sanitization or an existing target causes a collision, use `[SafeName]__[ShortAssetId].MMDRCV`. Never silently replace a different asset's card.

## Validity

`CardMissing`: no card exists. `CardValid`: container, manifest, declared preview (when present), asset ID and current source fingerprint agree. `CardStale`: valid card metadata points at a changed source. `CardBroken`: container or manifest cannot be read or validated. A valid card without a preview remains in the pending-card query and explicitly reports that thumbnail generation is pending. A stale card is kept until a successful atomic refresh.

## Thumbnail policy

Generated previews are 1024×1024 WebP, quality 50, square, framed around the full subject. The renderer version and preview settings are part of the cache key. A card may be created without `preview.webp` while renderer support is pending; its manifest explicitly records `thumbnail: null`. Writers validate archive entry names, manifest version, preview signature and source identity before publishing the temporary archive atomically.
