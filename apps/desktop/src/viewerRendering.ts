import * as THREE from "./vendor/three.module.js";

// Bound decoding/upload work: a large model must not start every texture at once.
export async function loadViewerTextures(paths: string[], active: () => boolean, load: (path: string) => Promise<void>) {
  const unique = [...new Set(paths.filter(Boolean))];
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(2, unique.length) }, async () => {
    while (active() && next < unique.length) await load(unique[next++]);
  }));
}

export function setMaterialAlpha(material: any, opacity: number, alphaMode = 0) {
  // PMX groups contain overlapping triangles; sorting a whole group cannot fix
  // their self-overdraw. Three's alpha hash keeps per-fragment depth instead.
  const alphaHash = opacity < 0.999 || alphaMode === 2;
  const alphaTest = alphaHash ? 0 : alphaMode === 1 ? 0.5 : 0;
  const coverage = alphaMode === 1 && !alphaHash;
  if (material.transparent || material.alphaHash !== alphaHash || material.alphaTest !== alphaTest || material.alphaToCoverage !== coverage) material.needsUpdate = true;
  material.transparent = false;
  material.alphaHash = alphaHash;
  material.opacity = opacity;
  material.depthWrite = true;
  material.alphaTest = alphaTest;
  material.alphaToCoverage = coverage;
}

// Three's default sort uses the mesh origin for every PMX material group.
// Keep each group's centre so front/back groups sort correctly when orbiting.
export function setMaterialCentre(material: any, geometry: any, start: number, count: number) {
  const centre = new THREE.Vector3();
  const position = geometry.getAttribute("position"), indices = geometry.index.array;
  for (let i = start; i < start + count; i++) {
    const vertex = indices[i];
    centre.x += position.getX(vertex); centre.y += position.getY(vertex); centre.z += position.getZ(vertex);
  }
  if (count) centre.divideScalar(count);
  material.userData.previewCentre = centre;
}

export function sortTransparentMaterials(renderer: any, camera: () => any) {
  const point = new THREE.Vector3();
  const depth = (item: any) => {
    const centre = item.material.userData.previewCentre;
    return centre ? -point.copy(centre).applyMatrix4(item.object.matrixWorld).applyMatrix4(camera().matrixWorldInverse).z : item.z;
  };
  renderer.setTransparentSort((a: any, b: any) =>
    a.groupOrder - b.groupOrder || a.renderOrder - b.renderOrder || depth(b) - depth(a) || a.id - b.id || (a.group?.start ?? 0) - (b.group?.start ?? 0));
}
