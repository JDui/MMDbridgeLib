import * as THREE from "./vendor/three.module.js";
import { readPreference } from "./preferences";

export const MATCAP_PREFERENCE = "mmdbridge-viewer-matcap-v1";
export const SCENE_PREFERENCE = "mmdbridge-viewer-scene-v1";
export const MATCAP_PRESETS = [
  { value: "original", label: "原材质" },
  { value: "soft", label: "柔光灰" },
  { value: "ceramic", label: "白瓷" },
  { value: "silver", label: "银灰" },
  { value: "copper", label: "暖铜" },
] as const;
export type MatcapPreset = typeof MATCAP_PRESETS[number]["value"];
export const SCENE_PRESETS = [
  { value: "original", label: "原始光照" },
  { value: "daylight", label: "柔和日光" },
  { value: "warm", label: "暖色室内" },
  { value: "night", label: "冷色夜景" },
  { value: "studio", label: "高对比" },
] as const;
export type ScenePreset = typeof SCENE_PRESETS[number]["value"];

export function readViewerPreset<T extends string>(key: string, presets: readonly { value: T }[]): T {
  const saved = readPreference(key);
  return presets.find((preset) => preset.value === saved)?.value ?? presets[0].value;
}

const finishes = {
  soft: { base: 0xb5bdc2, ambient: 0.34, diffuse: 0.63, specular: 0.12, gloss: 18, rim: 0.06 },
  ceramic: { base: 0xe6dfd5, ambient: 0.3, diffuse: 0.62, specular: 0.65, gloss: 72, rim: 0.08 },
  silver: { base: 0x80919d, ambient: 0.18, diffuse: 0.58, specular: 0.85, gloss: 44, rim: 0.28 },
  copper: { base: 0xb27650, ambient: 0.2, diffuse: 0.66, specular: 0.7, gloss: 36, rim: 0.16 },
};

function createMatcapTexture(preset: Exclude<MatcapPreset, "original">) {
  const finish = finishes[preset];
  const base = new THREE.Color(finish.base);
  const light = new THREE.Vector3(-0.45, 0.65, 0.75).normalize();
  const half = light.clone().add(new THREE.Vector3(0, 0, 1)).normalize();
  const size = 192;
  const pixels = new Uint8Array(size * size * 4);
  const encode = (value: number) => {
    const linear = Math.min(1, Math.max(0, value));
    return Math.round(255 * (linear <= 0.0031308 ? linear * 12.92 : 1.055 * Math.pow(linear, 1 / 2.4) - 0.055));
  };
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let nx = (x + 0.5) * 2 / size - 1;
      let ny = (y + 0.5) * 2 / size - 1;
      const radius = Math.hypot(nx, ny);
      if (radius > 1) { nx /= radius; ny /= radius; }
      const nz = Math.sqrt(Math.max(0, 1 - nx * nx - ny * ny));
      const diffuse = Math.max(0, nx * light.x + ny * light.y + nz * light.z);
      const specular = finish.specular * Math.pow(Math.max(0, nx * half.x + ny * half.y + nz * half.z), finish.gloss);
      const illumination = finish.ambient + finish.diffuse * diffuse + finish.rim * Math.pow(1 - nz, 3);
      const offset = (y * size + x) * 4;
      pixels[offset] = encode(base.r * illumination + specular);
      pixels[offset + 1] = encode(base.g * illumination + specular);
      pixels[offset + 2] = encode(base.b * illumination + specular);
      pixels[offset + 3] = 255;
    }
  }
  const texture = new THREE.DataTexture(pixels, size, size, THREE.RGBAFormat);
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.needsUpdate = true;
  return texture;
}

export type MatcapAppearance = { apply: (preset: MatcapPreset) => void; dispose: () => void };

export function createMatcapAppearance(mesh: any, originals: any[]): MatcapAppearance {
  const textures = new Map<string, any>();
  const materials = originals.map(() => {
    const material = new THREE.MeshMatcapMaterial({ color: 0xffffff, side: THREE.DoubleSide });
    // Keep texture transparency while making the finish independent of diffuse colour.
    material.onBeforeCompile = (shader: any) => {
      shader.fragmentShader = shader.fragmentShader.replace("#include <map_fragment>",
        "#ifdef USE_MAP\n diffuseColor.a *= texture2D(map, vMapUv).a;\n#endif");
    };
    material.customProgramCacheKey = () => "mbl-matcap-alpha-v1";
    return material;
  });
  const originalMaterial = originals.length === 1 ? originals[0] : originals;
  let disposed = false;
  return {
    apply(preset) {
      if (disposed) return;
      if (preset === "original") { mesh.material = originalMaterial; return; }
      let texture = textures.get(preset);
      if (!texture) { texture = createMatcapTexture(preset); textures.set(preset, texture); }
      materials.forEach((material: any, index: number) => {
        const source = originals[index];
        for (const property of ["map", "alphaMap", "opacity", "alphaTest", "alphaHash", "alphaToCoverage", "transparent", "depthWrite", "side", "polygonOffset", "polygonOffsetFactor", "polygonOffsetUnits"]) {
          if (material[property] !== source[property]) { material[property] = source[property]; material.needsUpdate = true; }
        }
        material.userData.previewCentre = source.userData.previewCentre;
        if (material.matcap !== texture) { material.matcap = texture; material.needsUpdate = true; }
      });
      mesh.material = materials.length === 1 ? materials[0] : materials;
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      mesh.material = originalMaterial;
      materials.forEach((material: any) => material.dispose());
      textures.forEach((texture) => texture.dispose());
      textures.clear();
    },
  };
}

export type SceneLighting = { renderer: any; ambient: any; key: any; fill: any; rim: any };
const sceneLooks = {
  original: { background: 0x10191f, exposure: 1, ambient: [0xffffff, 1.5], key: [0xffffff, 1.5], fill: [0xffffff, 0.7], rim: [0xffffff, 0.45] },
  daylight: { background: 0xb9ccd5, exposure: 0.95, ambient: [0xdceaff, 1.1], key: [0xfff2da, 3.1], fill: [0xbbd8ff, 1.2], rim: [0xffffff, 1] },
  warm: { background: 0x262019, exposure: 1, ambient: [0xd0b4a0, 0.7], key: [0xffc98c, 3.2], fill: [0x9fb7d2, 0.75], rim: [0xffe4c5, 1.2] },
  night: { background: 0x101a2d, exposure: 0.9, ambient: [0x8faad4, 0.5], key: [0xb6ccff, 2.1], fill: [0x718ed7, 0.65], rim: [0xf0b6d8, 1.5] },
  studio: { background: 0x151a20, exposure: 1.1, ambient: [0xffffff, 0.3], key: [0xffffff, 3.4], fill: [0xb6d4e0, 0.5], rim: [0xffffff, 2.1] },
};

export function applyScenePreset(lighting: SceneLighting, preset: ScenePreset) {
  const look = sceneLooks[preset];
  lighting.renderer.setClearColor(look.background, 1);
  lighting.renderer.toneMapping = preset === "original" ? THREE.NoToneMapping : THREE.ACESFilmicToneMapping;
  lighting.renderer.toneMappingExposure = look.exposure;
  for (const name of ["ambient", "key", "fill", "rim"] as const) {
    lighting[name].color.setHex(look[name][0]);
    lighting[name].intensity = look[name][1];
  }
}
