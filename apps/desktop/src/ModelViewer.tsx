import { ActionIcon, Button, Checkbox, NativeSelect, Slider, UnstyledButton, Modal } from "@mantine/core";
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import * as THREE from "./vendor/three.module.js";
import { OrbitControls } from "./vendor/OrbitControls.js";
import { toUiError } from "./uiError";
import { loadViewerTextures, setMaterialAlpha, setMaterialCentre, sortTransparentMaterials } from "./viewerRendering";
import "./model-viewer.css";

type ViewerAsset = { id?: string; name: string; primarySource: string; assetType?: "model" | "scene" };
type ViewerMode = "texture" | "materials" | "types" | "bone" | "anomaly";
type WeightType = { code: number; key: string; label: string; color: number; description: string };
type MaterialGroup = { start: number; count: number; color: [number, number, number, number]; texturePath: string };
type BoneInfo = { name: string; parent: number; position: [number, number, number] };
type ViewerStats = {
  counts: number[];
  influenceCounts: number[];
  degenerateSdef: number;
  nearDegenerateSdef: number;
  nonNormalized: number;
  singleInfluence: number;
  warnings: number[];
  sdefByBone: Array<{ index: number; name: string; count: number }>;
  qdefByBone: Array<{ index: number; name: string; count: number }>;
};
export type ParsedModel = {
  vertexCount: number;
  indices: Uint32Array;
  vertices: Float32Array;
  groups: MaterialGroup[];
  bones: BoneInfo[];
  stats: ViewerStats;
};
type SceneHandles = {
  renderer: any;
  scene: any;
  camera: any;
  controls: any;
  modelRoot: any;
  grid: any;
  resizeObserver: ResizeObserver;
  frame: (geometry: any) => void;
};
type ModelObjects = {
  data: ParsedModel;
  mesh: any;
  pointCloud: any;
  sdefCenters: any;
  bonePoints: any;
  boneLines: any;
  warningPoints: any;
  wireframe: any;
  materials: any[];
  textures: Map<string, { texture: any; alphaMode: number }>;
  vertexColors: Float32Array;
  pointColors: Float32Array;
  selectedPoint: any | null;
  doubleClick: (event: MouseEvent) => void;
  colorKey?: string;
};

const WEIGHT_TYPES: WeightType[] = [
  { code: 0, key: "bdef1", label: "BDEF1", color: 0x7f8c9b, description: "单骨骼绑定" },
  { code: 1, key: "bdef2", label: "BDEF2", color: 0x2f9e6e, description: "双骨骼线性混合" },
  { code: 2, key: "bdef4", label: "BDEF4", color: 0x2f6feb, description: "四骨骼混合" },
  { code: 3, key: "sdef", label: "SDEF", color: 0xe5484d, description: "球面变形" },
  { code: 4, key: "qdef", label: "QDEF", color: 0xa259e6, description: "双四元数变形" },
];
const WARNING_COLOR = 0xf5a524;
const NEUTRAL_COLOR = 0xc9ced6;

export function parsePreview(buffer: ArrayBuffer, analyzeWeights = true): ParsedModel {
  if (buffer.byteLength < 24) throw new Error("模型预览数据不完整。");
  const view = new DataView(buffer);
  if (view.getUint8(0) !== 77 || view.getUint8(1) !== 77 || view.getUint8(2) !== 68 || view.getUint8(3) !== 86) {
    throw new Error("无法识别的模型预览数据。");
  }
  if (view.getUint32(4, true) !== 3) throw new Error("模型预览数据版本不受支持。");
  const vertexCount = view.getUint32(8, true);
  const indexCount = view.getUint32(12, true);
  const groupCount = view.getUint32(16, true);
  const boneCount = view.getUint32(20, true);
  const vertexOffset = 24;
  const indexOffset = vertexOffset + vertexCount * 104;
  const groupsOffset = indexOffset + indexCount * 4;
  const fixedEnd = groupsOffset + groupCount * 24;
  if (![indexOffset, groupsOffset, fixedEnd].every(Number.isSafeInteger) || fixedEnd > buffer.byteLength) {
    throw new Error("模型预览数据长度无效。");
  }
  const vertices = new Float32Array(buffer, vertexOffset, vertexCount * 26);
  const indices = new Uint32Array(buffer, indexOffset, indexCount);
  const groups: MaterialGroup[] = [];
  for (let index = 0; index < groupCount; index += 1) {
    const offset = groupsOffset + index * 24;
    const start = view.getUint32(offset, true);
    const count = view.getUint32(offset + 4, true);
    if (start + count > indexCount || count % 3 !== 0) throw new Error("材质索引范围超出模型数据。");
    groups.push({ start, count, texturePath: "", color: [
      view.getFloat32(offset + 8, true), view.getFloat32(offset + 12, true),
      view.getFloat32(offset + 16, true), view.getFloat32(offset + 20, true),
    ] });
  }
  const bones: BoneInfo[] = [];
  const decoder = new TextDecoder();
  let offset = fixedEnd;
  for (let index = 0; index < boneCount; index += 1) {
    if (offset + 20 > buffer.byteLength) throw new Error("骨骼信息不完整。");
    const parent = view.getInt32(offset, true);
    const position: [number, number, number] = [
      view.getFloat32(offset + 4, true), view.getFloat32(offset + 8, true), view.getFloat32(offset + 12, true),
    ];
    const nameLength = view.getUint32(offset + 16, true);
    offset += 20;
    if (offset + nameLength > buffer.byteLength) throw new Error("骨骼名称数据不完整。");
    const name = decoder.decode(new Uint8Array(buffer, offset, nameLength)).trim() || `骨骼 ${index}`;
    offset += nameLength;
    bones.push({ name, parent, position });
  }
  for (const group of groups) {
    if (offset + 4 > buffer.byteLength) throw new Error("贴图路径数据不完整。");
    const length = view.getUint32(offset, true);
    offset += 4;
    if (offset + length > buffer.byteLength) throw new Error("贴图路径数据不完整。");
    group.texturePath = decoder.decode(new Uint8Array(buffer, offset, length));
    offset += length;
  }
  if (offset !== buffer.byteLength) throw new Error("模型预览数据包含无法识别的尾部内容。");
  if (!vertexCount || !indexCount) throw new Error("PMX 中没有可显示的三角网格。");
  return { vertexCount, indices, vertices, groups, bones, stats: analyzeModel(vertices, analyzeWeights ? vertexCount : 0, bones) };
}

function analyzeModel(vertices: Float32Array, vertexCount: number, bones: BoneInfo[]): ViewerStats {
  const counts = [0, 0, 0, 0, 0];
  const influenceCounts = [0, 0, 0, 0, 0];
  const warnings: number[] = [];
  const sdefByBone = new Map<number, number>();
  const qdefByBone = new Map<number, number>();
  let degenerateSdef = 0;
  let nearDegenerateSdef = 0;
  let nonNormalized = 0;
  let singleInfluence = 0;
  for (let index = 0; index < vertexCount; index += 1) {
    const base = index * 26;
    const mode = Math.max(0, Math.min(4, Math.round(vertices[base + 16])));
    counts[mode] += 1;
    let sum = 0;
    let active = 0;
    for (let slot = 0; slot < 4; slot += 1) {
      const weight = vertices[base + 12 + slot];
      sum += weight;
      if (weight > 1e-6) active += 1;
    }
    influenceCounts[Math.min(active, 4)] += 1;
    let warning = Math.abs(sum - 1) > 1e-3;
    if (warning) nonNormalized += 1;
    if (mode === 3) {
      const r0 = Math.hypot(vertices[base + 20], vertices[base + 21], vertices[base + 22]);
      const r1 = Math.hypot(vertices[base + 23], vertices[base + 24], vertices[base + 25]);
      if (r0 < 1e-5 && r1 < 1e-5) {
        degenerateSdef += 1;
        warning = true;
      } else if (r0 < 1e-4 || r1 < 1e-4) {
        nearDegenerateSdef += 1;
      }
      let sdefBone = -1;
      let sdefWeight = 1e-6;
      for (let slot = 0; slot < 2; slot += 1) {
        const weight = vertices[base + 12 + slot];
        if (weight > sdefWeight) {
          sdefWeight = weight;
          sdefBone = Math.round(vertices[base + 8 + slot]);
        }
      }
      if (sdefBone >= 0) sdefByBone.set(sdefBone, (sdefByBone.get(sdefBone) ?? 0) + 1);
    }
    if (mode === 4) {
      let qdefBone = -1;
      let qdefWeight = 1e-6;
      for (let slot = 0; slot < 4; slot += 1) {
        const weight = vertices[base + 12 + slot];
        if (weight > qdefWeight) {
          qdefWeight = weight;
          qdefBone = Math.round(vertices[base + 8 + slot]);
        }
      }
      if (qdefBone >= 0) qdefByBone.set(qdefBone, (qdefByBone.get(qdefBone) ?? 0) + 1);
    }
    if ((mode === 1 || mode === 2 || mode === 4) && active === 1) {
      singleInfluence += 1;
      warning = true;
    }
    if (warning) warnings.push(index);
  }
  const topSdef = Array.from(sdefByBone.entries())
    .sort((left, right) => right[1] - left[1])
    .slice(0, 12)
    .map(([index, count]) => ({ index, count, name: bones[index]?.name ?? `骨骼 ${index}` }));
  const topQdef = Array.from(qdefByBone.entries())
    .sort((left, right) => right[1] - left[1])
    .slice(0, 12)
    .map(([index, count]) => ({ index, count, name: bones[index]?.name ?? `骨骼 ${index}` }));
  return { counts, influenceCounts, degenerateSdef, nearDegenerateSdef, nonNormalized, singleInfluence, warnings, sdefByBone: topSdef, qdefByBone: topQdef };
}

function setHeatColor(target: Float32Array, offset: number, value: number) {
  const weight = Math.max(0, Math.min(1, value));
  if (weight < 0.001) {
    target[offset] = 0.06; target[offset + 1] = 0.09; target[offset + 2] = 0.14;
  } else if (weight < 0.5) {
    const t = weight * 2;
    target[offset] = 0.08 * (1 - t); target[offset + 1] = 0.27 + 0.64 * t; target[offset + 2] = 0.86 * (1 - t) + 0.1 * t;
  } else {
    const t = (weight - 0.5) * 2;
    target[offset] = 0.08 + 0.92 * t; target[offset + 1] = 0.91 * (1 - t) + 0.13 * t; target[offset + 2] = 0.1 * (1 - t);
  }
}

function safeFileName(value: string): string {
  return value.replace(/[<>:"/\\|?*\x00-\x1f]/g, "_").trim() || "model";
}

export default function ModelViewer({ asset, onClose }: { asset: ViewerAsset; onClose: () => void }) {
  const host = useRef<HTMLDivElement>(null);
  const handles = useRef<SceneHandles | null>(null);
  const objects = useRef<ModelObjects | null>(null);
  const [model, setModel] = useState<ParsedModel | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [mode, setMode] = useState<ViewerMode>("texture");
  const [texturesLoaded, setTexturesLoaded] = useState(0);
  const [visibleTypes, setVisibleTypes] = useState<Set<number>>(() => new Set(WEIGHT_TYPES.map((type) => type.code)));
  const [selectedBone, setSelectedBone] = useState(0);
  const [selectedVertex, setSelectedVertex] = useState<number | null>(null);
  const [showPoints, setShowPoints] = useState(false);
  const [showShadedMesh, setShowShadedMesh] = useState(false);
  const [showBones, setShowBones] = useState(false);
  const [showWireframe, setShowWireframe] = useState(false);
  const [showSdefCenters, setShowSdefCenters] = useState(false);
  const [pointSize, setPointSize] = useState(2.5);

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    let renderer: any;
    let controls: any;
    let resizeObserver: ResizeObserver | null = null;
    let disposeFlyControls = () => {};
    try {
      renderer = new THREE.WebGLRenderer({ antialias: true, preserveDrawingBuffer: true, logarithmicDepthBuffer: asset.assetType === "scene" });
      renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
      renderer.setClearColor(0x10191f, 1);
      renderer.outputColorSpace = THREE.SRGBColorSpace;
      element.appendChild(renderer.domElement);
      const scene = new THREE.Scene();
      const camera = new THREE.PerspectiveCamera(42, 1, 0.1, 4000);
      sortTransparentMaterials(renderer, () => camera);
      camera.position.set(0, 10, -28);
      controls = new OrbitControls(camera, renderer.domElement);
      controls.enableDamping = true;
      controls.dampingFactor = 0.08;
      controls.rotateSpeed = 0.85;
      controls.zoomSpeed = 0.45;
      controls.zoomToCursor = true;
      scene.add(new THREE.AmbientLight(0xffffff, 1.5));
      const keyLight = new THREE.DirectionalLight(0xffffff, 1.5);
      keyLight.position.set(-14, 26, -20);
      scene.add(keyLight);
      const fillLight = new THREE.DirectionalLight(0xffffff, 0.7);
      fillLight.position.set(18, 6, 16);
      scene.add(fillLight);
      const rimLight = new THREE.DirectionalLight(0xffffff, 0.45);
      rimLight.position.set(0, -14, 20);
      scene.add(rimLight);
      const grid = new THREE.GridHelper(60, 60, 0x344b52, 0x24363e);
      scene.add(grid);
      const modelRoot = new THREE.Group();
      scene.add(modelRoot);
      const resize = () => {
        if (!element.clientWidth || !element.clientHeight) return;
        camera.aspect = element.clientWidth / element.clientHeight;
        camera.updateProjectionMatrix();
        renderer.setSize(element.clientWidth, element.clientHeight);
      };
      resizeObserver = new ResizeObserver(resize);
      resizeObserver.observe(element);
      resize();
      const isScene = asset.assetType === "scene";
      let flySpeed = 10;
      const pressed = new Set<string>();
      let dragPointer: number | null = null;
      let lastX = 0, lastY = 0;
      let yaw = 0, pitch = -5 * Math.PI / 180;
      const orient = () => camera.quaternion.setFromEuler(new THREE.Euler(pitch, yaw, 0, "YXZ"));
      if (isScene) {
        controls.enabled = false;
        renderer.domElement.tabIndex = 0;
        const syncModifiers = (event: { shiftKey: boolean; ctrlKey: boolean }) => {
          if (event.shiftKey) pressed.add("Shift"); else pressed.delete("Shift");
          if (event.ctrlKey) pressed.add("Control"); else pressed.delete("Control");
        };
        const keyDown = (event: KeyboardEvent) => {
          if (event.target instanceof HTMLElement && (event.target.matches("input,select,textarea,button") || event.target.isContentEditable)) return;
          if (!["KeyW", "KeyA", "KeyS", "KeyD", "KeyQ", "KeyE", "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight"].includes(event.code)) return;
          event.preventDefault(); syncModifiers(event); pressed.add(event.code);
        };
        const keyUp = (event: KeyboardEvent) => { pressed.delete(event.code); syncModifiers(event); };
        let disposed = false;
        let dragButton = 0;
        const releaseLock = () => {
          if (document.pointerLockElement === renderer.domElement) document.exitPointerLock();
        };
        const up = () => {
          const pointer = dragPointer;
          dragPointer = null;
          if (pointer !== null && renderer.domElement.hasPointerCapture(pointer)) renderer.domElement.releasePointerCapture(pointer);
          releaseLock();
        };
        const clear = () => { pressed.clear(); up(); };
        const lockError = () => {
          if (disposed || dragPointer === null) return;
          clear();
          console.warn("场景视口无法锁定鼠标，请松开后重新按下鼠标。");
        };
        const lockChange = () => {
          if (document.pointerLockElement === renderer.domElement) {
            // The button may have been released before the async lock completed.
            if (disposed || dragPointer === null) releaseLock();
          } else if (dragPointer !== null) clear();
        };
        const down = (event: PointerEvent) => {
          if ((event.button !== 0 && event.button !== 2) || dragPointer !== null) return;
          event.preventDefault(); syncModifiers(event); renderer.domElement.focus({ preventScroll: true });
          dragPointer = event.pointerId; dragButton = event.button; lastX = event.clientX; lastY = event.clientY;
          if (event.pointerType === "mouse") {
            try {
              const request = renderer.domElement.requestPointerLock();
              request?.then(() => { if (disposed || dragPointer === null) releaseLock(); }).catch(lockError);
            } catch { lockError(); }
          } else renderer.domElement.setPointerCapture(event.pointerId);
        };
        const rotate = (dx: number, dy: number) => {
          yaw -= dx * 0.003;
          pitch = Math.max(-Math.PI / 2 + 0.01, Math.min(Math.PI / 2 - 0.01, pitch - dy * 0.003));
          orient();
        };
        const move = (event: PointerEvent) => {
          if (dragPointer !== event.pointerId || document.pointerLockElement === renderer.domElement) return;
          rotate(event.clientX - lastX, event.clientY - lastY);
          lastX = event.clientX; lastY = event.clientY;
        };
        const lockedMove = (event: MouseEvent) => {
          if (dragPointer !== null && document.pointerLockElement === renderer.domElement) rotate(event.movementX, event.movementY);
        };
        const mouseUp = (event: MouseEvent) => { if (event.button === dragButton) up(); };
        const context = (event: Event) => event.preventDefault();
        const wheel = (event: WheelEvent) => {
          event.preventDefault();
          const pixels = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? element.clientHeight : 1);
          camera.fov = Math.max(20, Math.min(110, camera.fov * Math.exp(Math.max(-500, Math.min(500, pixels)) * 0.0015)));
          camera.updateProjectionMatrix();
        };
        window.addEventListener("keydown", keyDown); window.addEventListener("keyup", keyUp); window.addEventListener("blur", clear);
        document.addEventListener("pointerlockchange", lockChange); document.addEventListener("pointerlockerror", lockError);
        document.addEventListener("mousemove", lockedMove); document.addEventListener("mouseup", mouseUp);
        renderer.domElement.addEventListener("pointerdown", down); renderer.domElement.addEventListener("pointermove", move);
        renderer.domElement.addEventListener("pointerup", up); renderer.domElement.addEventListener("pointercancel", up); renderer.domElement.addEventListener("lostpointercapture", up);
        renderer.domElement.addEventListener("contextmenu", context);
        renderer.domElement.addEventListener("wheel", wheel, { passive: false });
        disposeFlyControls = () => {
          disposed = true;
          clear(); window.removeEventListener("keydown", keyDown); window.removeEventListener("keyup", keyUp); window.removeEventListener("blur", clear);
          document.removeEventListener("pointerlockchange", lockChange); document.removeEventListener("pointerlockerror", lockError);
          document.removeEventListener("mousemove", lockedMove); document.removeEventListener("mouseup", mouseUp);
          renderer.domElement.removeEventListener("pointerdown", down); renderer.domElement.removeEventListener("pointermove", move);
          renderer.domElement.removeEventListener("pointerup", up); renderer.domElement.removeEventListener("pointercancel", up); renderer.domElement.removeEventListener("lostpointercapture", up);
          renderer.domElement.removeEventListener("contextmenu", context);
          renderer.domElement.removeEventListener("wheel", wheel);
        };
      }
      const frame = (geometry: any) => {
        if (isScene) {
          geometry.computeBoundingSphere();
          camera.position.set(0, 165 / 8, 0);
          yaw = 0; pitch = -5 * Math.PI / 180; orient();
          camera.fov = 90; camera.near = 0.1;
          const positions = geometry.attributes.position;
          let far = 250;
          for (let i = 0; i < positions.count; i++) {
            const distance = Math.hypot(positions.getX(i), positions.getY(i) - 165 / 8, positions.getZ(i));
            if (Number.isFinite(distance)) far = Math.max(far, distance);
          }
          camera.far = Math.min(far * 1.25, 100000); camera.updateProjectionMatrix();
          flySpeed = Math.max(2, Math.min(100, geometry.boundingSphere.radius * 0.08));
          grid.position.set(0, 0, 0); grid.scale.setScalar(Math.max(geometry.boundingSphere.radius / 30, 1));
          return;
        }
        geometry.computeBoundingSphere();
        const sphere = geometry.boundingSphere;
        const radius = Math.max(sphere.radius, 0.001);
        const halfFov = (camera.fov * Math.PI / 180) / 2;
        const homeDistance = radius / Math.sin(halfFov) * 0.85;
        controls.target.copy(sphere.center);
        camera.position.set(sphere.center.x, sphere.center.y + radius * 0.12, sphere.center.z - homeDistance);
        controls.minDistance = Math.max(radius * 0.35, 0.01);
        controls.maxDistance = radius * 4.5;
        camera.near = Math.max(radius * 0.01, 0.001);
        camera.far = controls.maxDistance + radius * 4;
        camera.updateProjectionMatrix();
        controls.update();
        grid.position.y = sphere.center.y - radius * 1.02;
        grid.scale.setScalar(Math.max(radius / 15, 0.01));
      };
      const sceneHandles: SceneHandles = { renderer, scene, camera, controls, modelRoot, grid, resizeObserver, frame };
      handles.current = sceneHandles;
      let previousTime = performance.now();
      const movement = new THREE.Vector3();
      renderer.setAnimationLoop(() => {
        const now = performance.now();
        const delta = Math.min((now - previousTime) / 1000, 0.05); previousTime = now;
        if (isScene) {
          movement.set(Number(pressed.has("KeyD")) - Number(pressed.has("KeyA")), 0, Number(pressed.has("KeyS")) - Number(pressed.has("KeyW")));
          movement.applyQuaternion(camera.quaternion);
          movement.y += Number(pressed.has("KeyE")) - Number(pressed.has("KeyQ"));
          const speedMultiplier = pressed.has("Control") ? 0.25 : pressed.has("Shift") ? 4 : 1;
          if (movement.lengthSq()) camera.position.addScaledVector(movement.normalize(), flySpeed * speedMultiplier * delta);
        } else controls.update();
        renderer.render(scene, camera);
      });
    } catch (reason) {
      setError(toUiError(reason));
      setLoading(false);
    }
    return () => {
      if (objects.current) disposeModelObjects(objects.current, handles.current);
      objects.current = null;
      disposeFlyControls();
      resizeObserver?.disconnect();
      if (renderer) {
        renderer.setAnimationLoop(null);
        renderer.dispose();
        renderer.domElement.remove();
      }
      controls?.dispose();
      handles.current = null;
    };
  }, [asset.assetType]);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError("");
    setModel(null);
    setSelectedVertex(null);
    setTexturesLoaded(0);
    const command = asset.assetType === "scene" ? "scene_preview" : asset.id ? "model_preview" : "model_preview_file";
    const args = asset.id ? { assetId: asset.id } : { path: asset.primarySource };
    invoke<ArrayBuffer>(command, args)
      .then((buffer) => {
        if (!active) return;
        const parsed = parsePreview(buffer);
        const scene = handles.current;
        if (!scene) throw new Error("3D 渲染窗口尚未准备好。");
        const geometry = new THREE.BufferGeometry();
        const interleaved = new THREE.InterleavedBuffer(parsed.vertices, 26);
        geometry.setAttribute("position", new THREE.InterleavedBufferAttribute(interleaved, 3, 0));
        geometry.setAttribute("normal", new THREE.InterleavedBufferAttribute(interleaved, 3, 3));
        geometry.setAttribute("uv", new THREE.InterleavedBufferAttribute(interleaved, 2, 6));
        const vertexColors = new Float32Array(parsed.vertexCount * 3);
        const pointColors = new Float32Array(parsed.vertexCount * 3);
        geometry.setAttribute("color", new THREE.BufferAttribute(vertexColors, 3));
        geometry.setIndex(new THREE.BufferAttribute(parsed.indices, 1));
        const groups = parsed.groups.length ? parsed.groups : [{ start: 0, count: parsed.indices.length, color: [0.72, 0.76, 0.79, 1] as [number, number, number, number], texturePath: "" }];
        const materials = groups.map((group) => new THREE.MeshStandardMaterial({
          color: new THREE.Color().setRGB(group.color[0], group.color[1], group.color[2]),
          vertexColors: true,
          roughness: 0.72,
          metalness: 0,
          side: THREE.DoubleSide,
        }));
        groups.forEach((group, index) => geometry.addGroup(group.start, group.count, index));
        const mesh = new THREE.Mesh(geometry, materials.length === 1 ? materials[0] : materials);
        mesh.frustumCulled = false;
        scene.modelRoot.add(mesh);

        const pointGeometry = new THREE.BufferGeometry();
        pointGeometry.setAttribute("position", geometry.getAttribute("position"));
        pointGeometry.setAttribute("color", new THREE.BufferAttribute(pointColors, 3));
        const pointCloud = new THREE.Points(pointGeometry, new THREE.PointsMaterial({
          size: pointSize,
          sizeAttenuation: false,
          vertexColors: true,
          depthTest: true,
        }));
        pointCloud.frustumCulled = false;
        pointCloud.visible = false;
        scene.modelRoot.add(pointCloud);

        const sdefCoordinates: number[] = [];
        for (let index = 0; index < parsed.vertexCount; index += 1) {
          const base = index * 26;
          if (Math.round(parsed.vertices[base + 16]) === 3) {
            sdefCoordinates.push(parsed.vertices[base + 17], parsed.vertices[base + 18], parsed.vertices[base + 19]);
          }
        }
        const sdefGeometry = new THREE.BufferGeometry();
        sdefGeometry.setAttribute("position", new THREE.Float32BufferAttribute(sdefCoordinates, 3));
        const sdefCenters = new THREE.Points(sdefGeometry, new THREE.PointsMaterial({
          color: 0x101418,
          size: pointSize * 1.25,
          sizeAttenuation: false,
          transparent: true,
          opacity: 0.78,
          depthTest: true,
        }));
        sdefCenters.frustumCulled = false;
        sdefCenters.visible = false;
        scene.modelRoot.add(sdefCenters);

        const warningCoordinates: number[] = [];
        for (const index of parsed.stats.warnings) {
          const base = index * 26;
          warningCoordinates.push(parsed.vertices[base], parsed.vertices[base + 1], parsed.vertices[base + 2]);
        }
        const warningGeometry = new THREE.BufferGeometry();
        warningGeometry.setAttribute("position", new THREE.Float32BufferAttribute(warningCoordinates, 3));
        const warningPoints = new THREE.Points(warningGeometry, new THREE.PointsMaterial({
          color: WARNING_COLOR,
          size: pointSize * 2,
          sizeAttenuation: false,
          depthTest: true,
        }));
        warningPoints.frustumCulled = false;
        warningPoints.visible = false;
        scene.modelRoot.add(warningPoints);

        const bonePositions = new Float32Array(parsed.bones.length * 3);
        const segments: number[] = [];
        parsed.bones.forEach((bone, index) => {
          bonePositions.set(bone.position, index * 3);
          if (bone.parent >= 0 && bone.parent < parsed.bones.length) {
            segments.push(...bone.position, ...parsed.bones[bone.parent].position);
          }
        });
        const boneGeometry = new THREE.BufferGeometry();
        boneGeometry.setAttribute("position", new THREE.BufferAttribute(bonePositions, 3));
        const bonePoints = new THREE.Points(boneGeometry, new THREE.PointsMaterial({
          color: 0x42a7ff,
          size: 4.5,
          sizeAttenuation: false,
          depthTest: false,
        }));
        bonePoints.visible = false;
        bonePoints.renderOrder = 999;
        scene.modelRoot.add(bonePoints);
        const lineGeometry = new THREE.BufferGeometry();
        lineGeometry.setAttribute("position", new THREE.Float32BufferAttribute(segments, 3));
        const boneLines = new THREE.LineSegments(lineGeometry, new THREE.LineBasicMaterial({
          color: 0x42a7ff,
          transparent: true,
          opacity: 0.34,
          depthTest: false,
        }));
        boneLines.visible = false;
        boneLines.renderOrder = 998;
        scene.modelRoot.add(boneLines);

        // The expensive edge deduplication is only needed when wireframe is enabled.
        const wireGeometry = new THREE.BufferGeometry();
        const wireframe = new THREE.LineSegments(wireGeometry, new THREE.LineBasicMaterial({
          color: 0x85979e,
          transparent: true,
          opacity: 0.12,
        }));
        wireframe.visible = false;
        wireframe.material.depthWrite = false;
        wireframe.frustumCulled = false;
        scene.modelRoot.add(wireframe);

        let selectedPoint: any | null = null;
        const raycaster = new THREE.Raycaster();
        const mouse = new THREE.Vector2();
        const doubleClick = (event: MouseEvent) => {
          const rect = scene.renderer.domElement.getBoundingClientRect();
          mouse.x = ((event.clientX - rect.left) / rect.width) * 2 - 1;
          mouse.y = -((event.clientY - rect.top) / rect.height) * 2 + 1;
          raycaster.setFromCamera(mouse, scene.camera);
          const hit = raycaster.intersectObject(mesh, false)[0];
          if (!hit?.face) return;
          const faceIndices = [hit.face.a, hit.face.b, hit.face.c];
          let nearest = faceIndices[0];
          let nearestDistance = Infinity;
          for (const vertex of faceIndices) {
            const base = vertex * 26;
            const dx = parsed.vertices[base] - hit.point.x;
            const dy = parsed.vertices[base + 1] - hit.point.y;
            const dz = parsed.vertices[base + 2] - hit.point.z;
            const distance = dx * dx + dy * dy + dz * dz;
            if (distance < nearestDistance) { nearest = vertex; nearestDistance = distance; }
          }
          setSelectedVertex(nearest);
        };
        scene.renderer.domElement.addEventListener("dblclick", doubleClick);
        objects.current = {
          data: parsed, mesh, pointCloud, sdefCenters, bonePoints, boneLines, warningPoints, wireframe,
          materials, textures: new Map(), vertexColors, pointColors, selectedPoint, doubleClick,
        };
        scene.frame(geometry);
        setModel(parsed);
        setLoading(false);
        parsed.groups.forEach((group, index) => setMaterialCentre(materials[index], geometry, group.start, group.count));
        const texturePaths = parsed.groups.map((group) => group.texturePath);
        void (async () => {
          const loader = new THREE.TextureLoader();
          await loadViewerTextures(texturePaths, () => active, async (texturePath) => {
            try {
              const bytes = await invoke<ArrayBuffer>(asset.assetType === "scene" ? "scene_preview_texture" : "model_preview_texture_file",
                asset.assetType === "scene" ? { assetId: asset.id, texturePath } : { modelPath: asset.primarySource, texturePath });
              if (!active || bytes.byteLength <= 1) return;
              const alphaMode = new Uint8Array(bytes)[0];
              const url = URL.createObjectURL(new Blob([new Uint8Array(bytes, 1)], { type: "image/png" }));
              try {
                const texture = await loader.loadAsync(url);
                texture.colorSpace = THREE.SRGBColorSpace;
                texture.flipY = false;
                texture.wrapS = THREE.RepeatWrapping;
                texture.wrapT = THREE.RepeatWrapping;
                texture.needsUpdate = true;
                if (active && objects.current?.data === parsed) {
                  objects.current.textures.set(texturePath, { texture, alphaMode });
                  setTexturesLoaded((count) => count + 1);
                } else texture.dispose();
              } finally { URL.revokeObjectURL(url); }
            } catch (reason) { console.warn("模型贴图无法读取", texturePath, reason); }
          });
        })();
      })
      .catch((reason: unknown) => {
        if (active) {
          setError(toUiError(reason));
          setLoading(false);
        }
      });
    return () => {
      active = false;
      if (objects.current) disposeModelObjects(objects.current, handles.current);
      objects.current = null;
    };
  }, [asset.id, asset.primarySource, asset.assetType]);

  useEffect(() => {
    const current = objects.current;
    if (!current) return;
    const { data, mesh, pointCloud, sdefCenters, bonePoints, boneLines, warningPoints, wireframe, materials } = current;
    const colorKey = `${mode}:${selectedBone}:${[...visibleTypes].sort().join(",")}`;
    if (current.colorKey !== colorKey) {
      const typeColors = WEIGHT_TYPES.map((type) => new THREE.Color(type.color).toArray());
      const neutral = new THREE.Color(NEUTRAL_COLOR).toArray();
      for (let index = 0; index < data.vertexCount; index += 1) {
        const base = index * 26;
        const modeCode = Math.max(0, Math.min(4, Math.round(data.vertices[base + 16])));
        let rgb: [number, number, number];
        if (mode === "materials" || mode === "texture") {
          rgb = typeColors[modeCode];
        } else if (mode === "anomaly" || (mode === "bone" && selectedBone < 0)) {
          rgb = neutral;
        } else if (mode === "bone") {
          let weight = 0;
          for (let slot = 0; slot < 4; slot += 1) {
            if (Math.round(data.vertices[base + 8 + slot]) === selectedBone) weight += data.vertices[base + 12 + slot];
          }
          const offset = index * 3;
          setHeatColor(current.vertexColors, offset, weight);
          current.pointColors[offset] = current.vertexColors[offset];
          current.pointColors[offset + 1] = current.vertexColors[offset + 1];
          current.pointColors[offset + 2] = current.vertexColors[offset + 2];
          continue;
        } else {
          rgb = visibleTypes.has(modeCode) ? typeColors[modeCode] : neutral;
        }
        const offset = index * 3;
        current.vertexColors[offset] = rgb[0]; current.vertexColors[offset + 1] = rgb[1]; current.vertexColors[offset + 2] = rgb[2];
        current.pointColors[offset] = rgb[0]; current.pointColors[offset + 1] = rgb[1]; current.pointColors[offset + 2] = rgb[2];
      }
      mesh.geometry.getAttribute("color").needsUpdate = true;
      pointCloud.geometry.getAttribute("color").needsUpdate = true;
      current.colorKey = colorKey;
    }
    const pointMode = showPoints;
    mesh.visible = !pointMode || showShadedMesh;
    pointCloud.visible = pointMode;
    pointCloud.material.size = pointSize;
    warningPoints.material.size = pointSize * 2;
    sdefCenters.material.size = pointSize * 1.25;
    warningPoints.visible = mode === "anomaly";
    sdefCenters.visible = showSdefCenters && visibleTypes.has(3) && mode !== "anomaly";
    bonePoints.visible = showBones;
    boneLines.visible = showBones;
    if (showWireframe && !wireframe.geometry.getAttribute("position")) {
      wireframe.geometry.dispose();
      wireframe.geometry = new THREE.WireframeGeometry(mesh.geometry);
    }
    wireframe.visible = showWireframe;
    for (let index = 0; index < materials.length; index += 1) {
      const material = materials[index];
      const group = data.groups[index];
      if (mode === "materials" || mode === "texture") {
        material.color.setRGB(group?.color[0] ?? 0.72, group?.color[1] ?? 0.76, group?.color[2] ?? 0.79);
      } else {
        material.color.setRGB(1, 1, 1);
      }
      const useVertexColors = mode !== "materials" && mode !== "texture";
      if (material.vertexColors !== useVertexColors) {
        material.vertexColors = useVertexColors;
        material.needsUpdate = true;
      }
      const textureRecord = mode === "texture" ? current.textures.get(group?.texturePath) : undefined;
      const texture = textureRecord?.texture ?? null;
      if (material.map !== texture) { material.map = texture; material.needsUpdate = true; }
      setMaterialAlpha(material, (mode === "texture" || mode === "materials") ? group?.color[3] ?? 1 : 1, textureRecord?.alphaMode ?? 0);
      material.polygonOffset = showWireframe;
      material.polygonOffsetFactor = 1;
      material.polygonOffsetUnits = 1;
    }
    if (selectedVertex !== null && selectedVertex >= 0 && selectedVertex < data.vertexCount) {
      const offset = selectedVertex * 26;
      const coordinate = new Float32Array([data.vertices[offset], data.vertices[offset + 1], data.vertices[offset + 2]]);
      if (!current.selectedPoint) {
        const geometry = new THREE.BufferGeometry();
        geometry.setAttribute("position", new THREE.BufferAttribute(coordinate, 3));
        current.selectedPoint = new THREE.Points(geometry, new THREE.PointsMaterial({ color: WARNING_COLOR, size: 9, sizeAttenuation: false, depthTest: false }));
        current.selectedPoint.renderOrder = 1000;
        handles.current?.modelRoot.add(current.selectedPoint);
      } else {
        current.selectedPoint.geometry.getAttribute("position").array.set(coordinate);
        current.selectedPoint.geometry.getAttribute("position").needsUpdate = true;
      }
    } else if (current.selectedPoint) {
      handles.current?.modelRoot.remove(current.selectedPoint);
      current.selectedPoint.geometry.dispose();
      current.selectedPoint.material.dispose();
      current.selectedPoint = null;
    }
  }, [mode, texturesLoaded, visibleTypes, selectedBone, selectedVertex, showPoints, showShadedMesh, showBones, showWireframe, showSdefCenters, pointSize, model]);


  const selectedVertexDetails = model && selectedVertex !== null ? (() => {
    const base = selectedVertex * 26;
    const modeCode = Math.round(model.vertices[base + 16]);
    const influences = [] as Array<{ bone: string; weight: number }>;
    for (let slot = 0; slot < 4; slot += 1) {
      const weight = model.vertices[base + 12 + slot];
      if (weight > 1e-6) {
        const bone = Math.round(model.vertices[base + 8 + slot]);
        influences.push({ bone: model.bones[bone]?.name ?? `骨骼 ${bone}`, weight });
      }
    }
    return {
      mode: WEIGHT_TYPES[modeCode]?.label ?? "未知",
      influences,
      center: [17, 18, 19].map((part) => model.vertices[base + part]),
      r0: [20, 21, 22].map((part) => model.vertices[base + part]),
      r1: [23, 24, 25].map((part) => model.vertices[base + part]),
    };
  })() : null;

  function toggleWeightType(code: number) {
    setVisibleTypes((current) => {
      const next = new Set(current);
      if (next.has(code)) next.delete(code); else next.add(code);
      return next;
    });
  }

  function showOnlyWeightType(code: number) {
    setMode("types");
    setVisibleTypes(new Set([code]));
  }

  function showAllWeightTypes() {
    setVisibleTypes(new Set(WEIGHT_TYPES.map((type) => type.code)));
  }

  function exportPng() {
    const renderer = handles.current?.renderer;
    if (!renderer) return;
    const link = document.createElement("a");
    link.href = renderer.domElement.toDataURL("image/png");
    link.download = `${safeFileName(asset.name)}-3d-preview.png`;
    link.click();
  }

  function resetCamera() {
    const current = objects.current;
    if (current) handles.current?.frame(current.mesh.geometry);
  }

  const totalWarnings = model?.stats.warnings.length ?? 0;
  const modeHelp: Record<ViewerMode, string> = {
    texture: "显示 PMX 材质贴图；贴图会在网格出现后逐张加载。",
    materials: "按 PMX 漫反射色查看各材质分区。",
    types: "按 BDEF / SDEF / QDEF 类型着色；点击图例可淡化或恢复对应类型。",
    bone: "选择骨骼后显示该骨骼在每个顶点上的权重，0 到 1 使用热力色阶。",
    anomaly: "高亮权重和偏离 1、零半径 SDEF 和无效多骨骼绑定。",
  };

  return <Modal.Root opened onClose={onClose} withinPortal={false} centered xOffset={20} yOffset={20} size={1120} zIndex={250} padding={0} transitionProps={{ duration: 150 }}><Modal.Overlay backgroundOpacity={0.72} blur={6} /><Modal.Content className="viewer-modal-content" aria-label={`${asset.name} 3D 查看器`}><Modal.Body p={0} className={`model-viewer-window ${asset.assetType === "scene" ? "scene-viewer" : ""}`}><header className="model-viewer-header">
        <div><span className="model-viewer-eyebrow">{asset.assetType === "scene" ? "场景预览" : "模型与权重预览"}</span><h2 title={asset.name}>{asset.name}</h2><small title={asset.primarySource}>{asset.primarySource}</small></div>
        <div className="model-viewer-header-actions"><Button disabled={!model} title="导出当前视角 PNG" onClick={exportPng}>导出截图</Button><ActionIcon variant="subtle" size="sm" className="model-viewer-close" aria-label="关闭 3D 预览" onClick={onClose}>×</ActionIcon></div>
      </header><div className="model-viewer-content">
        <aside className="model-viewer-sidebar">
          <div className="model-viewer-sidebar-scroll">
            <section className="mv-section">
              <h3>查看模式</h3>
              <div className="mv-mode-grid">
                {([ ["texture", "材质贴图"], ["types", "权重类型"], ["materials", "材质颜色"], ["bone", "骨骼权重"], ["anomaly", "异常顶点"] ] as Array<[ViewerMode, string]>).map(([value, label]) => <Button key={value} variant={mode === value ? "filled" : "light"} aria-pressed={mode === value} onClick={() => setMode(value)}>{label}</Button>)}
              </div>
              <p className="mv-hint">{modeHelp[mode]}</p>
              {mode === "texture" && model && <p className="mv-hint">已加载贴图 {texturesLoaded} / {new Set(model.groups.map((group) => group.texturePath).filter(Boolean)).size}</p>}
              {mode === "bone" && <label className="mv-select-label">目标骨骼<NativeSelect value={selectedBone} onChange={(event) => setSelectedBone(Number(event.target.value))} disabled={!model?.bones.length}>{model?.bones.map((bone, index) => <option value={index} key={`${index}-${bone.name}`}>{bone.name} ({index})</option>)}</NativeSelect></label>}
            </section>
            <section className="mv-section">
              <h3>权重类型 <span>点击图例切换高亮</span></h3>
              <div className="mv-weight-shortcuts" role="group" aria-label="快速筛选权重类型">
                <Button disabled={!model?.stats.counts[3]} onClick={() => showOnlyWeightType(3)}>只看 SDEF</Button>
                <Button disabled={!model?.stats.counts[4]} onClick={() => showOnlyWeightType(4)}>只看 QDEF</Button>
                <Button disabled={visibleTypes.size === WEIGHT_TYPES.length} onClick={showAllWeightTypes}>全部类型</Button>
              </div>
              <div className="mv-weight-legend">{WEIGHT_TYPES.map((type) => <UnstyledButton key={type.code} className={!visibleTypes.has(type.code) ? "muted" : ""} onClick={() => toggleWeightType(type.code)} title={type.description}>
                <i style={{ backgroundColor: `#${type.color.toString(16).padStart(6, "0")}` }} /><span>{type.label}</span><b>{(model?.stats.counts[type.code] ?? 0).toLocaleString()}</b>
              </UnstyledButton>)}</div>
            </section>
            {model && <section className="mv-section">
              <h3>模型统计</h3>
              <div className="mv-stat"><span>顶点 / 面</span><b>{model.vertexCount.toLocaleString()} / {Math.floor(model.indices.length / 3).toLocaleString()}</b></div>
              <div className="mv-stat"><span>骨骼</span><b>{model.bones.length.toLocaleString()}</b></div>
              <div className="mv-stat"><span>SDEF 顶点</span><b className="mv-accent-sdef">{model.stats.counts[3].toLocaleString()} ({(model.stats.counts[3] * 100 / model.vertexCount).toFixed(2)}%)</b></div>
              <div className="mv-stat"><span>QDEF 顶点</span><b className="mv-accent-qdef">{model.stats.counts[4].toLocaleString()}</b></div>
              <div className="mv-stat"><span>影响骨骼数</span><b>{model.stats.influenceCounts.slice(1).map((count) => count.toLocaleString()).join(" / ")}</b></div>
            </section>}
            {model && <section className="mv-section">
              <h3>权重审计</h3>
              <div className="mv-stat"><span>异常顶点</span><b className={totalWarnings ? "mv-warn" : "mv-ok"}>{totalWarnings.toLocaleString()}</b></div>
              <div className="mv-audit-copy">零半径 SDEF：{model.stats.degenerateSdef.toLocaleString()}<br />近退化 SDEF：{model.stats.nearDegenerateSdef.toLocaleString()}<br />权重和异常：{model.stats.nonNormalized.toLocaleString()}<br />单骨骼多权重：{model.stats.singleInfluence.toLocaleString()}</div>
            </section>}
            {model && model.stats.sdefByBone.length > 0 && <section className="mv-section">
              <h3>SDEF 热点骨骼</h3>
              <div className="mv-bone-hotlist">{model.stats.sdefByBone.map((bone) => <UnstyledButton key={bone.index} onClick={() => { setMode("bone"); setSelectedBone(bone.index); }} title="查看该骨骼的权重热图"><span>{bone.name}</span><i><b style={{ width: `${Math.max(4, bone.count * 100 / model.stats.sdefByBone[0].count)}%` }} /></i><strong>{bone.count.toLocaleString()}</strong></UnstyledButton>)}</div>
            </section>}
            {model && model.stats.qdefByBone.length > 0 && <section className="mv-section">
              <h3>QDEF 主影响骨骼 <span>按顶点主权重统计</span></h3>
              <div className="mv-bone-hotlist">{model.stats.qdefByBone.map((bone) => <UnstyledButton key={bone.index} onClick={() => { setMode("bone"); setSelectedBone(bone.index); }} title="查看该骨骼的权重热图，包含 QDEF 顶点"><span>{bone.name}</span><i><b style={{ width: `${Math.max(4, bone.count * 100 / model.stats.qdefByBone[0].count)}%` }} /></i><strong>{bone.count.toLocaleString()}</strong></UnstyledButton>)}</div>
            </section>}
            <section className="mv-section mv-view-options">
              <h3>显示选项</h3>
              <Checkbox  checked={showPoints} onChange={(event) => setShowPoints(event.target.checked)} label={<>顶点点云叠加</>} />
              {showPoints && <Checkbox className="mv-indent" checked={showShadedMesh} onChange={(event) => setShowShadedMesh(event.target.checked)} label={<>保留素模底色</>} />}
              <Checkbox  checked={showBones} onChange={(event) => setShowBones(event.target.checked)} label={<>显示骨骼与连线</>} />
              <Checkbox  checked={showWireframe} onChange={(event) => setShowWireframe(event.target.checked)} label={<>线框叠加</>} />
              <Checkbox  checked={showSdefCenters} onChange={(event) => setShowSdefCenters(event.target.checked)} label={<>显示 SDEF 球心 C</>} />
              <div className="mv-slider">点云尺寸 <b>{pointSize.toFixed(1)}</b><Slider thumbLabel="点云尺寸" min={1} max={8} step={0.5} value={pointSize} onChange={(value) => setPointSize(value)} /></div>
            </section>
            {selectedVertexDetails && selectedVertex !== null && <section className="mv-section mv-vertex-info">
              <h3>顶点 {selectedVertex.toLocaleString()} <Button onClick={() => setSelectedVertex(null)} aria-label="清除选中顶点">×</Button></h3>
              <div className="mv-stat"><span>权重类型</span><b>{selectedVertexDetails.mode}</b></div>
              {selectedVertexDetails.influences.map((item, index) => <div className="mv-stat" key={`${item.bone}-${index}`}><span>{item.bone}</span><b>{item.weight.toFixed(5)}</b></div>)}
              {selectedVertexDetails.mode === "SDEF" && <div className="mv-sdef-params"><b>C</b> {selectedVertexDetails.center.map((value) => value.toFixed(4)).join(", ")}<br /><b>R0</b> {selectedVertexDetails.r0.map((value) => value.toFixed(4)).join(", ")}<br /><b>R1</b> {selectedVertexDetails.r1.map((value) => value.toFixed(4)).join(", ")}</div>}
            </section>}
          </div>
        </aside>
        <main className="model-viewer-stage">
          <div className="model-viewer-canvas" ref={host} />
          {loading && <div className="model-viewer-message">正在由 Rust Core 解析 3D 网格…</div>}
          {error && <div className="model-viewer-message model-viewer-error">模型无法预览：{error}</div>}
          {!loading && !error && <><div className="model-viewer-hud">{model?.vertexCount.toLocaleString()} 顶点{asset.assetType === "scene" ? " · 场景网格" : " · 双击顶点查看权重数据"}</div><div className="model-viewer-controls">{asset.assetType === "scene" ? <><span>WASD 移动</span><span>Q 降 / E 升</span><span>Shift 加速 / Ctrl 慢速</span><span>左键 / 右键拖动朝向</span><span>滚轮调整 FOV</span></> : <><span>左键旋转</span><span>滚轮缩放</span><span>右键平移</span></>}<Button onClick={resetCamera}>重置视角</Button></div></>}
        </main>
      </div><footer className="model-viewer-footer">{asset.assetType === "scene" ? "场景以源文件的世界坐标和材质显示。" : "已解析 PMX 权重与材质贴图；当前不执行骨骼姿势或物理模拟。"}</footer></Modal.Body></Modal.Content></Modal.Root>;
}

function disposeModelObjects(objects: ModelObjects, handles: SceneHandles | null) {
  for (const record of objects.textures.values()) record.texture.dispose();
  const rendererElement = handles?.renderer?.domElement as HTMLCanvasElement | undefined;
  if (rendererElement) rendererElement.removeEventListener("dblclick", objects.doubleClick);
  if (objects.selectedPoint) {
    handles?.modelRoot.remove(objects.selectedPoint);
    objects.selectedPoint.geometry.dispose();
    objects.selectedPoint.material.dispose();
  }
  const owned = [objects.mesh, objects.pointCloud, objects.sdefCenters, objects.bonePoints, objects.boneLines, objects.warningPoints, objects.wireframe];
  for (const object of owned) {
    if (!object) continue;
    handles?.modelRoot.remove(object);
    object.geometry?.dispose();
    if (Array.isArray(object.material)) object.material.forEach((material: any) => material.dispose());
    else object.material?.dispose();
  }
}
