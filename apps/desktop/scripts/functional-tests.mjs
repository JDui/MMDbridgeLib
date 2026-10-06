import assert from "node:assert/strict";
import { test, after } from "node:test";
import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { build } from "esbuild";

const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const temporary = await mkdtemp(join(tmpdir(), "mmdbridge-functional-"));
after(async () => { await rm(temporary, { recursive: true }); });
const bundle = await build({
  stdin: { contents: 'export * from "./src/preferences"; export * from "./src/libraryState"; export * from "./src/frameRequests"; export * from "./src/previewCamera"; export * from "./src/viewerPresets"; export * as Three from "./src/vendor/three.module.js";', resolveDir: app, loader: "ts" },
  bundle: true, platform: "node", format: "esm", write: false,
});
const modulePath = join(temporary, "functions.mjs");
await writeFile(modulePath, bundle.outputFiles[0].text);
const { readPreference, writePreference, readAssetViewState, visibleSelection, activeThumbnailStatuses, createFrameRequestQueue, sphereCameraDistance,
  createMatcapAppearance, applyScenePreset, Three } = await import(pathToFileURL(modulePath).href);

test("Matcap switches preserve cutout, opacity, geometry and skinning, including late textures", () => {
  const geometry = new Three.BufferGeometry();
  const original = new Three.MeshStandardMaterial({ opacity: 0.6, alphaHash: true, alphaTest: 0.5, vertexColors: true });
  const mesh = new Three.SkinnedMesh(geometry, original);
  const skeleton = new Three.Skeleton([new Three.Bone()]); mesh.bind(skeleton);
  const appearance = createMatcapAppearance(mesh, [original]);
  const map = new Three.DataTexture(new Uint8Array([50,100,150,0]),1,1,Three.RGBAFormat);
  appearance.apply("ceramic");
  const matcap = mesh.material;
  assert.ok(matcap.isMeshMatcapMaterial); assert.equal(matcap.vertexColors,false);
  assert.equal(matcap.opacity,0.6); assert.equal(matcap.alphaTest,0.5); assert.equal(matcap.alphaHash,true);
  original.map = map; appearance.apply("ceramic"); assert.equal(matcap.map,map);
  const shader = { fragmentShader: "#include <map_fragment>" }; matcap.onBeforeCompile(shader);
  assert.match(shader.fragmentShader,/diffuseColor\.a/); assert.doesNotMatch(shader.fragmentShader,/diffuseColor\.rgb/);
  appearance.apply("silver"); assert.notEqual(matcap.matcap,null);
  assert.equal(mesh.geometry,geometry); assert.equal(mesh.skeleton,skeleton);
  appearance.apply("original"); assert.equal(mesh.material,original); assert.equal(original.map,map);
  let disposed = 0; matcap.addEventListener("dispose",()=>disposed++);
  appearance.dispose(); appearance.dispose(); assert.equal(disposed,1);
  geometry.dispose(); original.dispose(); map.dispose(); skeleton.dispose();
});

test("scene presets restore the original lighting and tone mapping", () => {
  const lighting = { renderer: { setClearColor(value){this.background=value;} }, ambient: new Three.AmbientLight(),
    key: new Three.DirectionalLight(), fill: new Three.DirectionalLight(), rim: new Three.DirectionalLight() };
  applyScenePreset(lighting,"warm"); const warm = lighting.key.color.getHex();
  applyScenePreset(lighting,"night"); assert.notEqual(lighting.key.color.getHex(),warm);
  applyScenePreset(lighting,"original"); assert.equal(lighting.renderer.toneMapping,Three.NoToneMapping);
  assert.equal(lighting.renderer.background,0x10191f); assert.equal(lighting.ambient.intensity,1.5);
});

test("initial camera fits a whole sphere in wide and narrow viewports", () => {
  for (const aspect of [0.35,0.55,1,1.3,2.1]) {
    const distance = sphereCameraDistance(4,42,aspect);
    const objectHalfAngle = Math.asin(4 / distance);
    const verticalHalf = 42 * Math.PI / 360;
    const horizontalHalf = Math.atan(Math.tan(verticalHalf) * aspect);
    assert.ok(objectHalfAngle < verticalHalf && objectHalfAngle < horizontalHalf, `clipped at aspect ${aspect}`);
  }
});

const storage = new Map();
function workingStorage() {
  globalThis.window = { localStorage: { getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value), removeItem: (key) => storage.delete(key) } };
}
workingStorage();
const savedView = { activeType: "all", activeMotionFormat: "vmd", activeRoot: null, activeDirectory: null, activeSavedFilterId: null, favoritesOnly: false, query: "雪ミク", searchText: "雪ミク", scrollTop: 1125, selectedTags: ["MMD", "雪"], tagMatch: "or", skeletonClass: "standard", recursiveScope: false };

test("blocked storage retains changes and removals for the current window", () => {
  globalThis.window = { get localStorage() { throw new Error("storage unavailable"); } };
  assert.equal(readPreference("blocked"), null);
  writePreference("blocked", "false");
  assert.equal(readPreference("blocked"), "false");
  writePreference("blocked", null);
  assert.equal(readPreference("blocked"), null);
  workingStorage();
});

test("quota failure does not restore an older preference", () => {
  storage.set("quota", "true");
  globalThis.window = { localStorage: { getItem: (key) => storage.get(key) ?? null, setItem: () => { throw new Error("quota"); } } };
  writePreference("quota", "false");
  assert.equal(readPreference("quota"), "false");
  workingStorage();
});

test("valid Unicode folder return state survives persistence", () => {
  writePreference("view", JSON.stringify(savedView));
  assert.deepEqual(readAssetViewState("view"), savedView);
});

test("older folder snapshots receive compatible filter defaults", () => {
  const { selectedTags, tagMatch, skeletonClass, recursiveScope, ...legacy } = savedView;
  writePreference("legacy-view", JSON.stringify(legacy));
  assert.deepEqual(readAssetViewState("legacy-view"), { ...legacy, selectedTags: [], tagMatch: "and", skeletonClass: "all", recursiveScope: true });
});

test("malformed or incomplete folder state cannot enter application state", () => {
  for (const invalid of ["{bad", "null", "[]", "42", JSON.stringify({ ...savedView, activeType: "x" }), JSON.stringify({ ...savedView, scrollTop: -2 }), JSON.stringify({ ...savedView, activeRoot: 5 }), JSON.stringify({ ...savedView, query: null })]) {
    writePreference("invalid-view", invalid);
    assert.equal(readAssetViewState("invalid-view"), null, invalid);
  }
});

test("refresh removes hidden bulk targets without changing the original selection", () => {
  const selected = new Set(["visible", "removed", "old-page"]);
  assert.deepEqual([...visibleSelection(selected, [{ id: "visible" }, { id: "other" }])], ["visible"]);
  assert.equal(selected.size, 3);
  assert.equal(visibleSelection(selected, []).size, 0);
});

test("cancelling remains active until a terminal worker acknowledgement", () => {
  assert.equal(activeThumbnailStatuses.has("Cancelling"), true);
  for (const terminal of ["Completed", "Failed", "Cancelled"]) assert.equal(activeThumbnailStatuses.has(terminal), false);
});

function deferred() {
  let resolvePromise, rejectPromise;
  const promise = new Promise((resolve, reject) => { resolvePromise = resolve; rejectPromise = reject; });
  return { promise, resolve: resolvePromise, reject: rejectPromise };
}

test("rapid seeks display only the latest requested frame", async () => {
  const first = deferred(), last = deferred(), requested = [], applied = [], errors = [];
  const queue = createFrameRequestQueue((frame) => { requested.push(frame); return frame === 1 ? first.promise : last.promise; }, (value) => applied.push(value), (error) => errors.push(error));
  const running = queue.request(1);
  await queue.request(2);
  await queue.request(3);
  first.resolve(1);
  await Promise.resolve();
  assert.deepEqual(requested, [1, 3]);
  assert.deepEqual(applied, []);
  last.resolve(3);
  await running;
  assert.deepEqual(applied, [3]);
  assert.deepEqual(errors, []);
  assert.equal(queue.pending, false);
});

test("closing a viewer discards its pending result and queued seek", async () => {
  const result = deferred(), applied = [], requested = [];
  const queue = createFrameRequestQueue((frame) => { requested.push(frame); return result.promise; }, (value) => applied.push(value), assert.fail);
  const running = queue.request(1);
  await queue.request(8);
  queue.dispose();
  result.resolve(1);
  await running;
  await queue.request(9);
  assert.deepEqual(requested, [1]);
  assert.deepEqual(applied, []);
});

test("an old viewer error cannot stop a newly opened viewer", async () => {
  const result = deferred(), oldErrors = [], applied = [];
  const old = createFrameRequestQueue(() => result.promise, assert.fail, (error) => oldErrors.push(error));
  const running = old.request(1);
  old.dispose();
  const current = createFrameRequestQueue(async (frame) => frame, (frame) => applied.push(frame), assert.fail);
  await current.request(20);
  result.reject(new Error("old request failed"));
  await running;
  assert.deepEqual(oldErrors, []);
  assert.deepEqual(applied, [20]);
});

test("failed frame loading releases the queue for retry", async () => {
  let fail = true;
  const applied = [], errors = [];
  const queue = createFrameRequestQueue(async (frame) => { if (fail) throw new Error("fixture"); return frame; }, (frame) => applied.push(frame), (error) => errors.push(error));
  await queue.request(1);
  assert.equal(queue.pending, false);
  fail = false;
  await queue.request(2);
  assert.equal(errors.length, 1);
  assert.deepEqual(applied, [2]);
});

test("every frontend command has a registered Tauri handler", async () => {
  const rust = await readFile(join(app, "src-tauri/src/main.rs"), "utf8");
  const registry = rust.match(/generate_handler!\[([\s\S]*?)\]/)?.[1];
  assert.ok(registry, "Tauri command registration is missing");
  const handlers = new Set(registry.match(/\b[a-z][a-z_]+\b/g));
  const files = (await readdir(join(app, "src"))).filter((name) => /\.(tsx?|js)$/.test(name));
  const missing = [];
  for (const file of files) {
    const source = await readFile(join(app, "src", file), "utf8");
    for (const match of source.matchAll(/\binvoke(?:<[^;\n]*?>)?\(\s*"([a-z_]+)"/g)) {
      if (!handlers.has(match[1])) missing.push(`${file}: ${match[1]}`);
    }
  }
  assert.deepEqual(missing, []);
});
