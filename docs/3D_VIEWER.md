# PMX 3D Preview and Weight Inspector

## User entry points

- Choose **打开 PMX** in the Library toolbar to select any local `.pmx` file for a one-off preview.
- Select a scanned Model asset and choose **3D 预览** in the inspector to preview its indexed source.
- The preview opens in a compact modal window. Drag to orbit, use the wheel to zoom, right-drag to pan, and double-click a mesh face to inspect its nearest vertex.

## Weight inspection

The viewer provides these modes:

- **权重类型** colors vertices as BDEF1, BDEF2, BDEF4, SDEF, or QDEF. The legend includes a count per type; clicking a type fades or restores its highlight.
- The quick filters isolate **SDEF** or **QDEF** in one step; **全部类型** restores the full palette. A filter is disabled when the PMX has no vertices of that type.
- **材质贴图** shows each material's PMX diffuse texture. The Core loads textures on demand, and the viewer uses PMX UV orientation, repeat wrapping, and the Core's alpha classification.
- **材质颜色** colors mesh groups with their PMX diffuse colors.
- **骨骼权重** paints each vertex by the selected bone's 0–1 influence. A vertex details panel lists all active bone influences.
- **异常顶点** overlays vertices with non-normalized weights, zero-radius SDEF parameters, or a single active influence in a multi-bone skinning type.

Optional overlays show the vertex point cloud, bone positions and parent links, wireframe, and SDEF center `C`. The SDEF hotspot list can switch directly to a high-count bone. The QDEF list counts each QDEF vertex under the bone with its largest individual influence; selecting a bone opens its full-model weight heat map, including any QDEF vertices it influences. Double-clicking a vertex shows its skinning type, influences, and SDEF `C`, `R0`, and `R1` values. A PNG export captures the current 3D view.

## Data path

```text
PMX file
  ↓
mmdbridge-core / mmd-anim-format PMX parser
  ↓
versioned MMDV binary IPC payload (mesh, material ranges, texture references, bones and weights)
  ↓
bundled Three.js + OrbitControls modal viewer
```

PMX parsing and weight extraction stay in Rust Core; TypeScript decodes only the Core-produced preview payload and renders its values. Three.js r160 and OrbitControls are bundled locally under their upstream MIT license headers, so the Viewer does not require a separately installed program or network access.

The payload carries PMX skinning mode, four bone indices and weights, plus SDEF `C` / `R0` / `R1` data. QDEF mode is carried separately from its four influences so QDEF vertices are distinguishable from BDEF4 while inspecting type colors and per-bone weights.

## Real-model validation

On 2026-09-25, `Library::model_preview_file` produced valid Core viewer payloads for nine real PMX files representing six distinct local models. The samples included `REM式プロセカ风初音ミクVS.pmx` (72,354 vertices, 2,240 SDEF), `EMT.pmx` (21,927 vertices, 150 SDEF), `泠鸢-舞台之星0122.pmx` (71,858 vertices, 9,146 SDEF), `石英式泠鸢yousa 折纸信笺版素体.pmx` (58,459 vertices, 6,505 SDEF), and `YYBmikubase.pmx` (35,300 vertices, 8,898 SDEF). All real samples contain zero QDEF vertices. A temporary EMT-derived PMX with one vertex changed to QDEF retained `QDEF=1` in the Core payload. Real-model frontend validation of QDEF shading, type switching and per-bone display remains unverified; the user chose to skip this validation for the current delivery. QDEF inspection support remains in the viewer.

The viewer's wheel zoom uses `0.075`.

## Scope limits

This viewer is a static mesh and weight inspector. It does not apply bone poses, execute SDEF/QDEF deformation, play VMD/VPD, simulate physics, or load sphere/toon maps. It supports PMX only and rejects files above 512 MiB or preview payloads above 256 MiB. It is not the thumbnail renderer: deterministic 1024 × 1024 WebP cards remain a separate native renderer stage.
