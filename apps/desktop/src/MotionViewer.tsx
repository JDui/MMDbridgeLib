import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import * as THREE from "./vendor/three.module.js";
import { OrbitControls } from "./vendor/OrbitControls.js";
import { parsePreview } from "./ModelViewer";
import "./motion-viewer.css";

type MotionAsset = { id: string; name: string; primarySource: string; metadata: Record<string, unknown> };
type CameraTrack = { distance: number; position: [number, number, number]; rotation: [number, number, number]; fov: number; perspective: boolean };
type MotionFrame = { frame: number; maxFrame: number; modelPath: string; boneMatrices: number[];
  ownCamera: CameraTrack | null; pairedCamera: CameraTrack | null; hasPairedCamera: boolean };
type CameraMode = "orbit" | "own" | "paired";

export default function MotionViewer({ asset, onClose }: { asset: MotionAsset; onClose: () => void }) {
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const host = useRef<HTMLDivElement>(null);
  const sceneRef = useRef<{ renderer: any; scene: any; camera: any; ortho: any; controls: any; mesh: any;
    bones: any[]; textures: any[]; resizeObserver: ResizeObserver } | null>(null);
  const requestPending = useRef(false);
  const wantedFrame = useRef<number | null>(null);
  const frameRef = useRef(0);
  const maxFrameRef = useRef(0);
  const playingRef = useRef(false);
  const speedRef = useRef(1);
  const cameraModeRef = useRef<CameraMode>("orbit");
  const lastTick = useRef(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [frame, setFrame] = useState(0);
  const [maxFrame, setMaxFrame] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  const [cameraMode, setCameraMode] = useState<CameraMode>("orbit");
  const [hasOwnCamera, setHasOwnCamera] = useState(false);
  const [hasPairedCamera, setHasPairedCamera] = useState(false);
  const lastCamera = useRef<MotionFrame | null>(null);
  const orbitState = useRef<{ position: any; target: any } | null>(null);

  function applyCamera(value: MotionFrame) {
    const view = sceneRef.current;
    if (!view) return;
    const camera = cameraModeRef.current === "own" ? value.ownCamera : cameraModeRef.current === "paired" ? value.pairedCamera : null;
    if (!camera) return;
    const target = new THREE.Vector3(...camera.position);
    const rotation = new THREE.Quaternion().setFromEuler(new THREE.Euler(-camera.rotation[0], -camera.rotation[1], -camera.rotation[2], "XYZ"));
    const eye = new THREE.Vector3(0, 0, -camera.distance).applyQuaternion(rotation).add(target);
    const up = new THREE.Vector3(0, 1, 0).applyQuaternion(rotation);
    const selected = camera.perspective ? view.camera : view.ortho;
    selected.position.copy(eye);
    selected.up.copy(up);
    selected.lookAt(target);
    if (camera.perspective) { selected.fov = Math.max(1, Math.min(179, camera.fov)); selected.far = Math.max(1000, Math.abs(camera.distance) * 10); }
    else {
      const halfHeight = Math.max(0.01, Math.abs(camera.distance) * Math.tan(Math.max(1, camera.fov) * Math.PI / 360));
      selected.top = halfHeight; selected.bottom = -halfHeight;
      selected.left = -halfHeight * view.camera.aspect; selected.right = halfHeight * view.camera.aspect;
    }
    selected.updateProjectionMatrix();
  }

  function applyFrame(value: MotionFrame) {
    const view = sceneRef.current;
    if (!view?.mesh || value.boneMatrices.length !== view.bones.length * 16) return;
    view.bones.forEach((bone, index) => {
      bone.matrix.fromArray(value.boneMatrices, index * 16);
      bone.matrixWorldNeedsUpdate = true;
    });
    view.mesh.updateMatrixWorld(true);
    lastCamera.current = value;
    applyCamera(value);
    frameRef.current = value.frame;
    maxFrameRef.current = value.maxFrame;
    setFrame(value.frame);
    setMaxFrame(value.maxFrame);
    setHasOwnCamera(value.ownCamera !== null);
    setHasPairedCamera(value.hasPairedCamera);
  }

  async function requestFrame(nextFrame: number) {
    wantedFrame.current = Math.max(0, Math.min(maxFrameRef.current, Math.round(nextFrame)));
    if (requestPending.current) return;
    requestPending.current = true;
    try {
      while (wantedFrame.current !== null) {
        const target = wantedFrame.current;
        wantedFrame.current = null;
        const value = await invoke<MotionFrame>("motion_preview_frame", { assetId: asset.id, frame: target });
        if (sceneRef.current) applyFrame(value);
      }
    } catch (reason) {
      playingRef.current = false;
      setPlaying(false);
      setError(String(reason));
    } finally { requestPending.current = false; }
  }

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    let disposed = false;
    const renderer = new THREE.WebGLRenderer({ antialias: true, preserveDrawingBuffer: true });
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    renderer.setClearColor(0x10191f, 1);
    renderer.outputColorSpace = THREE.SRGBColorSpace;
    element.appendChild(renderer.domElement);
    const scene = new THREE.Scene();
    const camera = new THREE.PerspectiveCamera(42, 1, 0.01, 100000);
    const ortho = new THREE.OrthographicCamera(-20, 20, 20, -20, 0.01, 100000);
    camera.position.set(0, 10, -28);
    const controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true; controls.dampingFactor = 0.08; controls.zoomSpeed = 0.075;
    scene.add(new THREE.AmbientLight(0xffffff, 1.5));
    const key = new THREE.DirectionalLight(0xffffff, 1.6); key.position.set(-14, 26, -20); scene.add(key);
    const fill = new THREE.DirectionalLight(0xffffff, 0.65); fill.position.set(18, 8, 16); scene.add(fill);
    const grid = new THREE.GridHelper(60, 60, 0x344b52, 0x24363e); scene.add(grid);
    const resize = () => {
      if (!element.clientWidth || !element.clientHeight) return;
      camera.aspect = element.clientWidth / element.clientHeight;
      camera.updateProjectionMatrix();
      renderer.setSize(element.clientWidth, element.clientHeight);
      if (lastCamera.current) applyCamera(lastCamera.current);
    };
    const resizeObserver = new ResizeObserver(resize);
    resizeObserver.observe(element);
    sceneRef.current = { renderer, scene, camera, ortho, controls, mesh: null, bones: [], textures: [], resizeObserver };
    resize();
    renderer.setAnimationLoop((time: number) => {
      if (playingRef.current && maxFrameRef.current > 0 && !requestPending.current) {
        if (time - lastTick.current >= 1000 / (30 * speedRef.current)) {
          lastTick.current = time;
          void requestFrame(frameRef.current >= maxFrameRef.current ? 0 : frameRef.current + 1);
        }
      }
      const active = sceneRef.current;
      if (!active) return;
      controls.enabled = cameraModeRef.current === "orbit";
      if (controls.enabled) controls.update();
      const cameraState = cameraModeRef.current === "own" ? lastCamera.current?.ownCamera
        : cameraModeRef.current === "paired" ? lastCamera.current?.pairedCamera : null;
      renderer.render(scene, cameraState && !cameraState.perspective ? ortho : camera);
    });

    (async () => {
      try {
        const initial = await invoke<MotionFrame>("motion_preview_frame", { assetId: asset.id, frame: 0 });
        const buffer = await invoke<ArrayBuffer>("model_preview_file", { path: initial.modelPath });
        if (disposed) return;
        const parsed = parsePreview(buffer);
        const geometry = new THREE.BufferGeometry();
        const interleaved = new THREE.InterleavedBuffer(parsed.vertices, 26);
        geometry.setAttribute("position", new THREE.InterleavedBufferAttribute(interleaved, 3, 0));
        geometry.setAttribute("normal", new THREE.InterleavedBufferAttribute(interleaved, 3, 3));
        geometry.setAttribute("uv", new THREE.InterleavedBufferAttribute(interleaved, 2, 6));
        const skinIndex = new Uint16Array(parsed.vertexCount * 4);
        const skinWeight = new Float32Array(parsed.vertexCount * 4);
        for (let vertex = 0; vertex < parsed.vertexCount; vertex++) {
          const base = vertex * 26;
          for (let slot = 0; slot < 4; slot++) {
            skinIndex[vertex * 4 + slot] = Math.min(65535, Math.max(0, Math.round(parsed.vertices[base + 8 + slot])));
            skinWeight[vertex * 4 + slot] = parsed.vertices[base + 12 + slot];
          }
        }
        geometry.setAttribute("skinIndex", new THREE.BufferAttribute(skinIndex, 4));
        geometry.setAttribute("skinWeight", new THREE.BufferAttribute(skinWeight, 4));
        geometry.setIndex(new THREE.BufferAttribute(parsed.indices, 1));
        const groups = parsed.groups.length ? parsed.groups : [{ start: 0, count: parsed.indices.length, color: [0.72, 0.76, 0.79, 1] as [number,number,number,number], texturePath: "" }];
        const materials = groups.map((group, index) => {
          geometry.addGroup(group.start, group.count, index);
          return new THREE.MeshStandardMaterial({ color: new THREE.Color().setRGB(group.color[0], group.color[1], group.color[2]),
            side: THREE.DoubleSide, roughness: 0.72, transparent: group.color[3] < 0.999, opacity: group.color[3] });
        });
        const mesh = new THREE.SkinnedMesh(geometry, materials.length === 1 ? materials[0] : materials);
        mesh.frustumCulled = false;
        const bones = parsed.bones.map((bone) => {
          const node = new THREE.Bone();
          node.name = bone.name;
          node.matrixAutoUpdate = false;
          node.matrix.makeTranslation(...bone.position);
          mesh.add(node);
          return node;
        });
        scene.add(mesh);
        mesh.updateMatrixWorld(true);
        mesh.bind(new THREE.Skeleton(bones));
        const active = sceneRef.current;
        if (!active) return;
        active.mesh = mesh; active.bones = bones;
        geometry.computeBoundingSphere();
        const sphere = geometry.boundingSphere;
        const radius = Math.max(sphere.radius, 0.01);
        controls.target.copy(sphere.center);
        camera.position.set(sphere.center.x, sphere.center.y + radius * 0.12, sphere.center.z - radius / Math.sin(camera.fov * Math.PI / 360));
        camera.near = Math.max(0.01, radius * 0.002);
        camera.far = Math.max(1000, radius * 10);
        camera.updateProjectionMatrix();
        controls.update();
        grid.position.y = sphere.center.y - radius;
        grid.scale.setScalar(Math.max(radius / 15, 0.01));
        applyFrame(initial);
        setLoading(false);
        const loader = new THREE.TextureLoader();
        for (let index = 0; index < groups.length; index++) {
          const texturePath = groups[index].texturePath;
          if (!texturePath || disposed) continue;
          try {
            const bytes = await invoke<ArrayBuffer>("model_preview_texture_file", { modelPath: initial.modelPath, texturePath });
            if (disposed || bytes.byteLength <= 1) continue;
            const alphaMode = new Uint8Array(bytes)[0];
            const url = URL.createObjectURL(new Blob([bytes.slice(1)], { type: "image/png" }));
            try {
              const texture = await loader.loadAsync(url);
              texture.colorSpace = THREE.SRGBColorSpace; texture.flipY = false;
              texture.wrapS = THREE.RepeatWrapping; texture.wrapT = THREE.RepeatWrapping;
              if (disposed) { texture.dispose(); continue; }
              active.textures.push(texture);
              materials[index].map = texture;
              materials[index].alphaTest = alphaMode === 1 ? 0.5 : 0;
              materials[index].transparent = alphaMode === 2 || groups[index].color[3] < 0.999;
              materials[index].depthWrite = !materials[index].transparent;
              materials[index].needsUpdate = true;
            } finally { URL.revokeObjectURL(url); }
          } catch (reason) { console.warn("动作预览贴图加载失败", texturePath, reason); }
        }
      } catch (reason) { if (!disposed) { setError(String(reason)); setLoading(false); } }
    })();
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onCloseRef.current(); };
    window.addEventListener("keydown", onKey);
    return () => {
      disposed = true; playingRef.current = false;
      window.removeEventListener("keydown", onKey);
      renderer.setAnimationLoop(null);
      resizeObserver.disconnect();
      controls.dispose();
      const active = sceneRef.current;
      active?.textures.forEach((texture) => texture.dispose());
      if (active?.mesh) {
        active.mesh.geometry.dispose();
        const mats = Array.isArray(active.mesh.material) ? active.mesh.material : [active.mesh.material];
        mats.forEach((material: any) => material.dispose());
      }
      renderer.dispose(); renderer.domElement.remove(); sceneRef.current = null;
    };
  }, [asset.id]);

  function changeCamera(mode: CameraMode) {
    const view = sceneRef.current;
    if (view && cameraModeRef.current === "orbit" && mode !== "orbit") {
      orbitState.current = { position: view.camera.position.clone(), target: view.controls.target.clone() };
    }
    cameraModeRef.current = mode;
    setCameraMode(mode);
    if (view && mode === "orbit" && orbitState.current) {
      view.camera.position.copy(orbitState.current.position);
      view.camera.up.set(0, 1, 0);
      view.camera.fov = 42;
      view.camera.updateProjectionMatrix();
      view.controls.target.copy(orbitState.current.target);
      view.controls.update();
    } else if (lastCamera.current) applyCamera(lastCamera.current);
  }

  return <div className="model-viewer-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <section className="motion-viewer-window" role="dialog" aria-modal="true" aria-label={`${asset.name} VMD 3D 预览`}>
      <header className="model-viewer-header"><div><span className="model-viewer-eyebrow">VMD MOTION VIEWER</span><h2>{asset.name}</h2><small title={asset.primarySource}>{asset.primarySource}</small></div><button className="model-viewer-close" aria-label="关闭动作预览" onClick={onClose}>×</button></header>
      <div className="motion-viewer-stage" ref={host}>
        {loading && <div className="model-viewer-message">正在加载预览模型和 VMD 轨道…</div>}
        {error && <div className="model-viewer-message model-viewer-error">{error}</div>}
      </div>
      <footer className="motion-viewer-timeline">
        <button disabled={loading || !!error} onClick={() => { playingRef.current = !playingRef.current; setPlaying(playingRef.current); lastTick.current = 0; }}>{playing ? "暂停" : "播放"}</button>
        <input aria-label="VMD 时间轴" type="range" min={0} max={Math.max(1, maxFrame)} step={1} value={frame} disabled={loading || !!error} onChange={(event) => { playingRef.current = false; setPlaying(false); void requestFrame(Number(event.target.value)); }} />
        <span>{frame} / {maxFrame} 帧</span>
        <select aria-label="播放速度" value={speed} onChange={(event) => { const next = Number(event.target.value); speedRef.current = next; setSpeed(next); }}><option value={0.5}>0.5×</option><option value={1}>1×</option><option value={2}>2×</option></select>
        <select aria-label="镜头视角" value={cameraMode} onChange={(event) => changeCamera(event.target.value as CameraMode)}><option value="orbit">自由视角</option>{hasOwnCamera && <option value="own">VMD 镜头</option>}{hasPairedCamera && <option value="paired">配套镜头</option>}</select>
      </footer>
      <div className="motion-viewer-note">骨骼与 IK 连续播放；SDEF/QDEF 在实时查看中使用线性蒙皮近似，顶点与材质 Morph 尚未实时显示。</div>
    </section>
  </div>;
}
