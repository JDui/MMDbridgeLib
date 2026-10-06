import { ActionIcon, Button, Checkbox, NativeSelect, Slider, TextInput, UnstyledButton, Alert, AppShell, Badge, Menu, Modal, MultiSelect, Progress, Tabs } from "@mantine/core";
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowUpRight, Box, Boxes, Camera, Check, ChevronDown, ChevronRight, ChevronUp, Clapperboard, FileText, Folder, FolderOpen, Grid2X2, History, Layers3, ListTodo, Minus, MoreHorizontal, PanelRight, Play, Plus, RefreshCw, Search, Settings2, SlidersHorizontal, Star, X } from "lucide-react";
import { AppearanceSettings } from "./AppearanceSettings";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { VirtuosoGrid } from "react-virtuoso";
import { toUiError } from "./uiError";
import { LibraryContextMenu } from "./LibraryContextMenu";
import { useLibraryDialog } from "./LibraryDialog";
import { readPreference, writePreference } from "./preferences";
import { activeThumbnailStatuses, readAssetViewState, visibleSelection, type AssetViewState } from "./libraryState";
import "./virtualized-grid.css";

const ModelViewer = lazy(() => import("./ModelViewer"));
const MotionViewer = lazy(() => import("./MotionViewer"));

const thumbnailReadQueue: Array<() => void> = [];
let activeThumbnailReads = 0;

function loadCardThumbnail(assetId: string, signal: AbortSignal): Promise<ArrayBuffer> {
  return new Promise((resolve, reject) => {
    let started = false;
    const cancel = () => {
      if (started) return;
      const index = thumbnailReadQueue.indexOf(start);
      if (index >= 0) thumbnailReadQueue.splice(index, 1);
      reject(new Error("缩略图请求已取消"));
    };
    const start = () => {
      if (signal.aborted) { cancel(); return; }
      started = true;
      activeThumbnailReads++;
      const finish = () => {
        activeThumbnailReads--;
        signal.removeEventListener("abort", cancel);
        thumbnailReadQueue.shift()?.();
      };
      invoke<ArrayBuffer>("card_thumbnail", { assetId })
        .then((buffer) => { resolve(buffer); finish(); }, (reason) => { reject(reason); finish(); });
    };
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) { cancel(); return; }
    if (activeThumbnailReads < 4) start();
    else thumbnailReadQueue.push(start);
  });
}

function CardThumbnail({ assetId, alt, revision = 0 }: { assetId: string; alt: string; revision?: number }) {
  const [source, setSource] = useState("");
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setSource("");
    setFailed(false);
    let active = true;
    let objectUrl = "";
    const controller = new AbortController();
    void loadCardThumbnail(assetId, controller.signal)
      .then((buffer) => {
        if (!active) return;
        if (!buffer?.byteLength) { setFailed(true); return; }
        objectUrl = URL.createObjectURL(new Blob([buffer], { type: "image/webp" }));
        setSource(objectUrl);
      })
      .catch(() => { if (active) setFailed(true); });
    return () => {
      active = false;
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [assetId, revision]);

  return <div className="card-thumbnail-slot">
    {source ? <img className="card-thumbnail-image" src={source} alt={alt} onError={() => { setSource(""); setFailed(true); }} /> : <span className="thumbnail-load-error" data-tone={failed ? "error" : "info"}>{failed ? "缩略图读取失败" : "正在加载缩略图…"}</span>}
  </div>;
}

type AssetType = "model" | "motion" | "scene";
type MotionFormat = "all" | "vmd" | "vpd";
type Root = {
  id: string;
  assetType: AssetType;
  path: string;
  displayName: string;
  enabled: boolean;
  scanRecursive: boolean;
  scanStatus: string;
};
type Asset = {
  id: string;
  assetType: AssetType;
  rootId: string;
  name: string;
  primarySource: string;
  assetDirectory: string;
  metadata: Record<string, unknown>;
  statuses: string[];
  cardStatus: string;
  hasThumbnail: boolean;
  isFavorite: boolean;
  updatedAt: string;
};
type AssetCursor = { name: string; id: string };
type AssetPage = { items: Asset[]; nextCursor: AssetCursor | null };
type AssetCounts = { all: number; model: number; motion: number; scene: number; byRoot: Record<string, number> };
type AssetDirectory = { path: string; count: number };
type DirectoryPage = { path: string; visibleCount: number; childDirectories: AssetDirectory[]; adjusted: boolean };
type FolderTreeNode = { path: string; name: string; count: number; children: Map<string, FolderTreeNode> };
type AssetTag = { name: string; source: "user" | "agent" | "parser"; confidence: number | null };
type Job = { id: string; status: string; kind: string; progress: number; asset_id?: string | null; error?: { message?: string } | null };
type JobSummary = Record<string, number>;
type ScanState = { rootId: string; status: string; queueOrder: number; fullCheck: boolean; scope: "full" | "local" | "none"; progress: number; filesSeen: number; filesProcessed: number; error: string | null; updatedAt: string };
type StorageInfo = { path: string; databaseBytes: number; walBytes: number; databaseLimitBytes: number; walTargetBytes: number };
type ThumbnailConcurrencySettings = { parse: number | null; render: number | null; encode: number | null };
type AssetOperationAsset = { id: string; name: string; primarySource: string };
type AssetOperationSourceSnapshot = { path: string; fileSize: string; modifiedNs: string };
type AssetOperationDependencySnapshot = {
  assetId: string;
  reference: string | null;
  role: string;
  path: string | null;
  status: string;
  fileSize: string | null;
  modifiedNs: string | null;
};
type AssetOperationPlan = {
  operation: "move" | "rename" | "recycle" | "delete_model";
  assetIds: string[];
  sourcePaths: string[];
  destinationPaths: string[];
  destinationParent: string | null;
  newName: string | null;
  affectedAssets: AssetOperationAsset[];
  dependencyPaths: string[];
  sourceSnapshots: AssetOperationSourceSnapshot[];
  packageSnapshots: AssetOperationSourceSnapshot[];
  dependencySnapshots: AssetOperationDependencySnapshot[];
  deleteMode?: "folder" | "pmxOnly" | null;
  deleteReason?: string | null;
  pmxDirectories?: Array<{ path: string; pmxPaths: string[] }>;
  preservedPaths?: string[];
  warnings: string[];
  canExecute: boolean;
};
type AssetOperationJournalEntry = {
  id: string;
  operation: string;
  status: string;
  sourcePaths: string[];
  destinationPaths: string[];
  affectedAssetCount: number;
  createdAt: string;
  updatedAt: string;
  result: { message?: string; completedPaths?: string[]; uncertainPaths?: string[]; notStartedPaths?: string[]; indexUpdateFailed?: boolean } | null;
};
type AssetRelation = {
  id: string;
  relationType: string;
  sourceAsset: string;
  targetAsset: string;
  confidence: number;
  reason: Record<string, unknown>;
  confirmed: boolean;
};
type SelectedDetails = {
  assetId: string;
  revision: number;
  status: "loading" | "ready" | "failed";
  tags: AssetTag[];
  relations: AssetRelation[];
  error: string;
};
type FilterField = "assetType" | "rootId" | "directory" | "tag" | "favorite" | "cardStatus" | "duplicateStatus" | "relationStatus" | "recentlyAdded" | "recentlyModified" | "needsReview" | "polygonCount" | "boneCount" | "skeletonClass" | "hasThumbnail" | "hasCard" | "frameCount" | "duration" | "hasBoneMotion" | "hasMorphMotion" | "hasCamera" | "cameraOnly" | "pose" | "hasPairedCamera" | "fileType" | "width" | "depth" | "area";
type FilterOperator = "eq" | "ne" | "contains" | "gt" | "gte" | "lt" | "lte";
type FilterExpr =
  | { op: "and" | "or"; children: FilterExpr[] }
  | { op: "not"; child: FilterExpr }
  | { op: "rule"; field: FilterField; operator: FilterOperator; value: string | number | boolean };
type SavedFilter = { id: string; name: string; expression: FilterExpr; createdAt: string; updatedAt: string };
type BuilderRule = { field: FilterField; operator: FilterOperator; value: string; negate: boolean };
const categoryLabels: Record<AssetType, string> = { model: "模型", motion: "动作", scene: "场景" };
const categoryIcons = { model: Box, motion: Clapperboard, scene: Layers3 };
function AssetKindIcon({ type, size = 18 }: { type: AssetType; size?: number }) {
  const Icon = categoryIcons[type];
  return <Icon size={size} strokeWidth={1.6} aria-hidden="true" />;
}
function assetSummary(asset: Asset) {
  if (asset.assetType === "motion" && typeof asset.metadata.duration_seconds === "number") return `${asset.metadata.duration_seconds.toFixed(1)} 秒`;
  if (typeof asset.metadata.polygon_count === "number") return `${asset.metadata.polygon_count.toLocaleString()} 面`;
  if (typeof asset.metadata.total_frames === "number") return `${asset.metadata.total_frames.toLocaleString()} 帧`;
  return asset.primarySource.split(/[\\/]/).pop();
}
const filterFieldLabels: Record<FilterField, string> = {
  skeletonClass: "骨架分类", assetType: "资产类型", rootId: "资产根目录", directory: "目录", tag: "标签", favorite: "收藏",
  cardStatus: "资源卡状态", duplicateStatus: "已停用的重复项条件", relationStatus: "有关联", recentlyAdded: "添加时间",
  recentlyModified: "修改时间", needsReview: "已停用条件", polygonCount: "面数", boneCount: "骨骼数",
  hasThumbnail: "有缩略图", hasCard: "有资源卡", frameCount: "动作帧数", duration: "动作时长",
  hasBoneMotion: "包含骨骼动作", hasMorphMotion: "包含表情动作", hasCamera: "包含镜头", cameraOnly: "已停用的纯镜头条件",
  pose: "Pose", hasPairedCamera: "有配套 Camera", fileType: "文件格式", width: "场景宽度", depth: "场景深度", area: "场景面积",
};
const booleanFilterFields = new Set<FilterField>(["favorite", "relationStatus", "hasThumbnail", "hasCard", "hasBoneMotion", "hasMorphMotion", "hasCamera", "cameraOnly", "pose", "hasPairedCamera"]);
const numericFilterFields = new Set<FilterField>(["polygonCount", "boneCount", "frameCount", "duration", "width", "depth", "area"]);
const dateFilterFields = new Set<FilterField>(["recentlyAdded", "recentlyModified"]);
const metadataLabels: Record<string, string> = {
  polygon_count: "面数", vertex_count: "顶点数", bone_count: "骨骼数", material_count: "材质数",
  morph_count: "Morph 数", rigid_body_count: "刚体数", joint_count: "关节数", total_frames: "总帧数",
  duration_seconds: "时长（秒）", start_frame: "起始帧", end_frame: "结束帧", has_bone_motion: "骨骼动画",
  has_morph_motion: "表情动画", has_camera: "包含镜头", has_light: "包含灯光", is_camera_only: "纯镜头",
  is_pose: "Pose", width: "宽度（MMD 单位）", depth: "深度（MMD 单位）", area: "占地面积",
  file_type: "格式", pmx_version: "PMX 版本", pmd_version: "PMD 版本", preview_frame: "预览帧",
};

function formatValue(value: unknown): string {
  if (typeof value === "boolean") return value ? "是" : "否";
  if (typeof value === "number") return Number.isInteger(value) ? value.toLocaleString("zh-CN") : value.toFixed(2);
  return String(value);
}

function formatMiB(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function assetStatusText(statuses: string[]): string {
  return statuses.filter((status) => status !== "NeedsReview").map((status) => ({ Ready: "就绪", ParseFailed: "解析失败", MissingSource: "源文件缺失", Unsupported: "暂不支持" } as Record<string, string>)[status] ?? status).join(" · ") || "就绪";
}

function cardStatusText(status: string): string {
  return ({ CardValid: "资源卡有效", CardMissing: "资源卡缺失", CardStale: "资源卡需更新", CardBroken: "资源卡损坏" } as Record<string, string>)[status] ?? status;
}

function importantStatus(asset: Asset): { text: string; tone: "error" | "warning" | "info" } | null {
  for (const status of ["MissingSource", "ParseFailed", "Unsupported"]) {
    if (asset.statuses.includes(status)) return { text: assetStatusText([status]), tone: status === "Unsupported" ? "warning" : "error" };
  }
  if (asset.cardStatus !== "CardValid") return { text: cardStatusText(asset.cardStatus), tone: asset.cardStatus === "CardBroken" ? "error" : "warning" };
  return asset.hasThumbnail ? null : { text: "尚无缩略图", tone: "info" };
}

function operatorsFor(field: FilterField): FilterOperator[] {
  if (booleanFilterFields.has(field)) return ["eq", "ne"];
  if (numericFilterFields.has(field) || dateFilterFields.has(field)) return ["eq", "ne", "gt", "gte", "lt", "lte"];
  return ["eq", "ne", "contains"];
}

function hasRetiredFilterCondition(expression: FilterExpr): boolean {
  if (expression.op === "rule") {
    if (expression.field === "duplicateStatus" || expression.field === "cameraOnly" || expression.field === "needsReview") return true;
    return expression.field === "fileType" && typeof expression.value === "string"
      && expression.value.trim().replace(/^\.+/, "").toLowerCase() === "x";
  }
  if (expression.op === "not") return hasRetiredFilterCondition(expression.child);
  return expression.children.some(hasRetiredFilterCondition);
}

const operatorLabels: Record<FilterOperator, string> = {
  eq: "等于", ne: "不等于", contains: "包含", gt: "大于", gte: "至少", lt: "小于", lte: "至多",
};
const LIBRARY_PAGE_SIZE = 120;
const activeScanStatuses = new Set(["Pending", "Discovering", "Indexing", "Verifying", "Relations", "Pausing", "Cancelling"]);
const scanStatusLabels: Record<string, string> = {
  Pending: "排队中", Discovering: "发现文件", Indexing: "建立索引", Verifying: "检查资源卡",
  Relations: "分析关系", Pausing: "正在暂停", Paused: "已暂停",
  Cancelling: "正在停止", Completed: "已完成", Failed: "失败", Cancelled: "已停止",
};
const jobStatusLabels: Record<string, string> = {
  Pending: "排队中", Parsing: "解析中", Rendering: "渲染中", Encoding: "编码中",
  Completed: "已完成", Failed: "失败", Cancelled: "已取消", Cancelling: "正在取消",
};

function isPathWithinRoot(path: string, root: string): boolean {
  const normalize = (value: string) => value.replaceAll("/", "\\").replace(/[\\]+$/, "").toLowerCase();
  const normalizedPath = normalize(path);
  const normalizedRoot = normalize(root);
  return normalizedPath === normalizedRoot || normalizedPath.startsWith(`${normalizedRoot}\\`);
}

function sameDirectoryPath(left: string, right: string): boolean {
  const normalize = (value: string) => value.replaceAll("/", "\\").replace(/[\\]+$/, "").toLowerCase();
  return normalize(left) === normalize(right);
}

function buildFolderTree(rootPath: string, directories: AssetDirectory[]): FolderTreeNode[] {
  const root: FolderTreeNode = { path: rootPath, name: "", count: 0, children: new Map() };
  for (const directory of directories) {
    if (!isPathWithinRoot(directory.path, rootPath)) continue;
    const parts = directory.path.slice(rootPath.length).split(/[\\/]/).filter(Boolean);
    let parent = root;
    for (const part of parts) {
      const key = part.toLowerCase();
      let child = parent.children.get(key);
      if (!child) {
        child = { path: `${parent.path.replace(/[\\/]+$/, "")}\\${part}`, name: part, count: 0, children: new Map() };
        parent.children.set(key, child);
      }
      child.count += directory.count;
      parent = child;
    }
  }
  return Array.from(root.children.values());
}

export default function App() {
  const [roots, setRoots] = useState<Root[]>([]);
  const rootsRef = useRef<Root[]>([]);
  const [rootsLoaded, setRootsLoaded] = useState(false);
  const [assets, setAssets] = useState<Asset[]>([]);
  const assetsById = useMemo(() => new Map(assets.map((asset) => [asset.id, asset])), [assets]);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [jobSummary, setJobSummary] = useState<JobSummary>({});
  const [scanStates, setScanStates] = useState<ScanState[]>([]);
  const [scanQueueOpen, setScanQueueOpen] = useState(false);
  const [previewAsset, setPreviewAsset] = useState<Asset | null>(null);
  const [cardSize, setCardSize] = useState(() => {
    const saved = Number(readPreference("mmdbridge-card-size"));
    return Number.isFinite(saved) && saved >= 130 && saved <= 300 ? saved : 200;
  });
  const [storageInfo, setStorageInfo] = useState<StorageInfo | null>(null);
  const [selectedDetails, setSelectedDetails] = useState<SelectedDetails | null>(null);
  const [assetQueryError, setAssetQueryError] = useState("");
  const [nextAssetCursor, setNextAssetCursor] = useState<AssetCursor | null>(null);
  const [loadingNextPage, setLoadingNextPage] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [savedFilters, setSavedFilters] = useState<SavedFilter[]>([]);
  const [counts, setCounts] = useState<AssetCounts>({ all: 0, model: 0, motion: 0, scene: 0, byRoot: {} });
  const [activeType, setActiveType] = useState<AssetType | "all">("all");
  const [activeMotionFormat, setActiveMotionFormat] = useState<MotionFormat>("all");
  const [viewMode, setViewMode] = useState<"assets" | "folders">("assets");
  const [activeRoot, setActiveRoot] = useState<string | null>(null);
  const [activeDirectory, setActiveDirectory] = useState<string | null>(null);
  const [recursiveScope, setRecursiveScope] = useState(() => readPreference("mmdbridge-folder-recursive") !== "false");
  const [directoryPage, setDirectoryPage] = useState<DirectoryPage | null>(null);
  const [assetDirectories, setAssetDirectories] = useState<AssetDirectory[]>([]);
  const folderReturnState = useRef<AssetViewState | null>(readAssetViewState("mmdbridge-asset-view-return"));
  const pendingScrollRestore = useRef<{ top: number; afterRevision: number; retryBlocked?: boolean } | null>(null);
  const [assetPageRevision, setAssetPageRevision] = useState(0);
  const [activeSavedFilterId, setActiveSavedFilterId] = useState<string | null>(null);
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [filterBuilderOpen, setFilterBuilderOpen] = useState(false);
  const [filterName, setFilterName] = useState("");
  const [filterGroupOp, setFilterGroupOp] = useState<"and" | "or">("and");
  const [filterRules, setFilterRules] = useState<BuilderRule[]>([{ field: "assetType", operator: "eq", value: "motion", negate: false }]);
  const batchRegeneration = useRef(false);
  const [tagNames, setTagNames] = useState<string[]>([]);
  const [selectedTags, setSelectedTags] = useState<string[]>([]);
  const [tagMatch, setTagMatch] = useState<"and" | "or">("and");
  const [skeletonClass, setSkeletonClass] = useState<AssetViewState["skeletonClass"]>("all");
  const [thumbnailRevisions, setThumbnailRevisions] = useState<Record<string, number>>({});
  const quickExpression = useMemo<FilterExpr | null>(() => {
    const children: FilterExpr[] = [];
    if (selectedTags.length) children.push({ op: tagMatch, children: selectedTags.map((name) => ({ op: "rule", field: "tag", operator: "eq", value: name })) });
    if (skeletonClass !== "all") children.push({ op: "rule", field: "skeletonClass", operator: "eq", value: skeletonClass });
    return children.length ? { op: "and", children } : null;
  }, [selectedTags, tagMatch, skeletonClass]);
  useEffect(() => { if (activeType !== "all" && activeType !== "model") setSkeletonClass("all"); }, [activeType]);
  const [selected, setSelected] = useState<Asset | null>(null);
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [selectedDetailsRevision, setSelectedDetailsRevision] = useState(0);
  const currentDetails = selectedDetails?.assetId === selected?.id && selectedDetails?.revision === selectedDetailsRevision ? selectedDetails : null;
  const detailsReady = currentDetails?.status === "ready";
  const assetTags = detailsReady ? currentDetails.tags : [];
  const assetRelations = detailsReady ? currentDetails.relations : [];
  const selectedDetailsRef = useRef({ assetId: selected?.id, revision: selectedDetailsRevision, ready: detailsReady });
  selectedDetailsRef.current = { assetId: selected?.id, revision: selectedDetailsRevision, ready: detailsReady };
  const [assetMenu, setAssetMenu] = useState<{ asset: Asset; x: number; y: number } | null>(null);
  const dialogs = useLibraryDialog();
  const [rootMenu, setRootMenu] = useState<{ root: Root; x: number; y: number } | null>(null);
  const [addRootOpen, setAddRootOpen] = useState(false);
  const [addRootType, setAddRootType] = useState<AssetType>("model");
  const [addRootPath, setAddRootPath] = useState("");
  const [addRootName, setAddRootName] = useState("");
  const [addRootRecursive, setAddRootRecursive] = useState(true);
  const [addRootBusy, setAddRootBusy] = useState(false);
  const [bulkSelectMode, setBulkSelectMode] = useState(false);
  const [bulkSelectedIds, setBulkSelectedIds] = useState<Set<string>>(() => new Set());
  const [viewerAsset, setViewerAsset] = useState<{ id?: string; name: string; primarySource: string; assetType?: "model" | "scene" } | null>(null);
  const [motionViewerAsset, setMotionViewerAsset] = useState<Asset | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [motionPreviewModel, setMotionPreviewModel] = useState<string | null>(null);
  const [thumbnailConcurrencyDraft, setThumbnailConcurrencyDraft] = useState<ThumbnailConcurrencySettings>({ parse: null, render: null, encode: null });
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [settingsError, setSettingsError] = useState("");
  const [assetOperationPlan, setAssetOperationPlan] = useState<AssetOperationPlan | null>(null);
  const [assetOperationError, setAssetOperationError] = useState("");
  const [operationJournalOpen, setOperationJournalOpen] = useState(false);
  const [operationJournalError, setOperationJournalError] = useState("");
  const [operationJournal, setOperationJournal] = useState<AssetOperationJournalEntry[]>([]);
  const [libraryScrollParent, setLibraryScrollParent] = useState<HTMLDivElement | null>(null);
  const [folderBrowserCompact, setFolderBrowserCompact] = useState(false);
  const [query, setQuery] = useState("");
  const [searchText, setSearchText] = useState("");
  const [busy, setBusy] = useState(false);
  const [jobsExpanded, setJobsExpanded] = useState(false);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const assetReadError = useRef<string | null>(null);
  const assetQueryRevision = useRef(0);
  const assetPageLoading = useRef<number | null>(null);
  const loadMoreRef = useRef<HTMLDivElement>(null);
  const scanStatesRef = useRef<ScanState[]>([]);
  const bulkPmxModelIds = Array.from(bulkSelectedIds).filter((id) => {
    const asset = assetsById.get(id);
    return asset?.assetType === "model" && asset.primarySource.toLowerCase().endsWith(".pmx");
  });
  const canDeleteBulkPmx = bulkSelectedIds.size > 0 && bulkPmxModelIds.length === bulkSelectedIds.size;

  useEffect(() => { rootsRef.current = roots; }, [roots]);

  useEffect(() => { writePreference("mmdbridge-card-size", String(cardSize)); }, [cardSize]);
  useEffect(() => { writePreference("mmdbridge-view-mode", viewMode); }, [viewMode]);
  useEffect(() => {
    if (viewMode !== "folders") return;
    writePreference("mmdbridge-folder-root", activeRoot);
  }, [activeRoot, viewMode]);
  useEffect(() => {
    if (viewMode !== "folders") return;
    writePreference("mmdbridge-folder-path", activeDirectory);
  }, [activeDirectory, viewMode]);
  useEffect(() => { writePreference("mmdbridge-folder-recursive", String(recursiveScope)); }, [recursiveScope]);

  useEffect(() => {
    if (!assetMenu && !rootMenu) return;
    const close = () => { setAssetMenu(null); setRootMenu(null); };
    window.addEventListener("resize", close);
    return () => window.removeEventListener("resize", close);
  }, [assetMenu, rootMenu]);

  function openAssetMenu(event: React.MouseEvent<HTMLElement> | React.KeyboardEvent<HTMLElement>, asset: Asset) {
    event.preventDefault();
    event.stopPropagation();
    setSelected(asset);
    setInspectorOpen(true);
    setRootMenu(null);
    const bounds = event.currentTarget.getBoundingClientRect();
    setAssetMenu({
      asset,
      x: Math.max(8, Math.min("clientX" in event ? event.clientX : bounds.left + 8, window.innerWidth - 232)),
      y: Math.max(8, Math.min("clientY" in event ? event.clientY : bounds.bottom, window.innerHeight - 238)),
    });
  }

  function openRootMenu(event: React.MouseEvent<HTMLButtonElement>, root: Root) {
    event.preventDefault();
    event.stopPropagation();
    setAssetMenu(null);
    const bounds = event.currentTarget.getBoundingClientRect();
    setRootMenu({
      root,
      x: Math.max(8, Math.min(bounds.right, window.innerWidth - 268)),
      y: Math.max(8, Math.min(bounds.top, window.innerHeight - 340)),
    });
  }

  useEffect(() => {
    setBulkSelectedIds(new Set());
  }, [activeRoot, activeDirectory, activeSavedFilterId, activeType, activeMotionFormat, favoritesOnly, searchText, quickExpression, recursiveScope, viewMode]);

  const refresh = useCallback(async () => {
    const revision = ++assetQueryRevision.current;
    setIsRefreshing(true);
    setAssetQueryError("");
    setSelectedDetails(null);
    setSelectedDetailsRevision((value) => value + 1);
    assetPageLoading.current = null;
    setLoadingNextPage(false);
    setNextAssetCursor(null);
    setAssetMenu(null);
    setRootMenu(null);
    try {
      const requestDirectory = activeRoot && (viewMode === "folders" || activeDirectory)
        ? activeDirectory ?? rootsRef.current.find((root) => root.id === activeRoot)?.path ?? null
        : activeDirectory;
      const assetRequest = viewMode === "folders" && activeRoot && !rootsLoaded
        ? null
        : invoke<AssetPage>("assets_page", {
          assetType: activeType === "all" ? null : activeType,
          motionFormat: activeType === "motion" && activeMotionFormat !== "all" ? activeMotionFormat : null,
          query: searchText || null,
          rootId: activeRoot,
          directoryPath: requestDirectory,
          recursiveScope,
          favoritesOnly,
          filterId: activeSavedFilterId,
          expression: quickExpression,
          cursor: null,
          limit: LIBRARY_PAGE_SIZE,
        });
      const current = () => revision === assetQueryRevision.current;
      const reportError = (reason: unknown) => { if (current()) setError(toUiError(reason)); };
      let assetResponseSettled = !assetRequest;
      const assetTimeout = assetRequest ? window.setTimeout(() => {
        if (!current() || assetResponseSettled) return;
        setAssets([]);
        setSelected(null);
        setBulkSelectedIds(new Set());
        setIsRefreshing(false);
        setAssetQueryError("资产读取超时，请重新读取。");
        assetReadError.current = "资产读取超时，请点击“重新读取”重试。";
        setError(assetReadError.current);
      }, 10_000) : null;
      await Promise.allSettled([
        invoke<Root[]>("roots_list").then((value) => {
          if (!current()) return;
          rootsRef.current = value;
          setRoots(value);
          setRootsLoaded(true);
          if (activeRoot) {
            const root = value.find((item) => item.id === activeRoot);
            if (!root) {
              setActiveRoot(null);
              setActiveDirectory(null);
              setNotice("上次浏览的资产根目录已移除，请重新选择目录。");
            } else {
              setActiveType(root.assetType);
              if (activeDirectory && !isPathWithinRoot(activeDirectory, root.path)) {
                setActiveDirectory(null);
              } else if (activeDirectory && sameDirectoryPath(activeDirectory, root.path)) {
                setActiveDirectory(null);
              }
            }
          }
        }).catch(reportError),
        invoke<AssetCounts>("asset_counts").then((value) => { if (current()) setCounts(value); }).catch(reportError),
        activeRoot && (viewMode === "folders" || activeDirectory)
          ? invoke<DirectoryPage>("asset_directory_page", { rootId: activeRoot, path: activeDirectory, recursiveScope })
            .then((value) => {
              if (!current()) return;
              setDirectoryPage(value);
              if (value.adjusted) {
                setActiveDirectory(value.path === rootsRef.current.find((root) => root.id === activeRoot)?.path ? null : value.path);
                setNotice("当前文件夹已失效，已返回最近存在的上级目录。");
              }
            }).catch(reportError)
          : Promise.resolve(),
        activeRoot && viewMode === "folders"
          ? invoke<AssetDirectory[]>("asset_directories", { rootId: activeRoot })
            .then((value) => { if (current()) setAssetDirectories(value); }).catch(reportError)
          : Promise.resolve(),
        assetRequest ? assetRequest.then((page) => {
          if (!current()) return;
          const visible = page.items.filter((asset) => !activeRoot || asset.rootId === activeRoot);
          setAssets(visible);
          setBulkSelectedIds((ids) => visibleSelection(ids, visible));
          setNextAssetCursor(page.nextCursor);
          setAssetPageRevision(revision);
          setAssetQueryError("");
          void invoke("library_background_start").catch(reportError);
          setSelected((previous) => visible.find((asset) => asset.id === previous?.id) ?? null);
          const resolvedReadError = assetReadError.current;
          assetReadError.current = null;
          if (resolvedReadError) setError((previous) => previous === resolvedReadError ? "" : previous);
        }).catch((reason) => {
          if (current()) {
            setAssets([]);
            setSelected(null);
            setBulkSelectedIds(new Set());
            const message = toUiError(reason);
            assetReadError.current = message;
            setAssetQueryError(message);
            setError(message);
          }
        }).finally(() => {
          assetResponseSettled = true;
          if (assetTimeout !== null) window.clearTimeout(assetTimeout);
          if (current()) setIsRefreshing(false);
        }) : Promise.resolve(),
        invoke<Job[]>("jobs_list").then((value) => { if (current()) setJobs(value); }).catch(reportError),
        invoke<JobSummary>("jobs_summary").then((value) => { if (current()) setJobSummary(value); }).catch(reportError),
        invoke<ScanState[]>("scan_states").then((value) => {
          if (!current()) return;
          scanStatesRef.current = value;
          setScanStates(value);
        }).catch(reportError),
        invoke<string[]>("tags_list").then((value) => { if (current()) setTagNames(value); }).catch(reportError),
        invoke<SavedFilter[]>("filters_list").then((value) => { if (current()) setSavedFilters(value); }).catch(reportError),
      ]);
    } catch (reason) {
      if (revision === assetQueryRevision.current) setError(toUiError(reason));
    } finally {
      if (revision === assetQueryRevision.current) setIsRefreshing(false);
    }
  }, [activeRoot, activeDirectory, activeSavedFilterId, activeType, activeMotionFormat, favoritesOnly, searchText, viewMode, recursiveScope, rootsLoaded, quickExpression]);

  const loadNextAssetPage = useCallback(async () => {
    const cursor = nextAssetCursor;
    const revision = assetQueryRevision.current;
    if (!cursor || assetPageLoading.current === revision) return;
    assetPageLoading.current = revision;
    setLoadingNextPage(true);
    try {
      const requestDirectory = activeRoot && (viewMode === "folders" || activeDirectory)
        ? activeDirectory ?? rootsRef.current.find((root) => root.id === activeRoot)?.path ?? null
        : activeDirectory;
      const page = await invoke<AssetPage>("assets_page", {
          assetType: activeType === "all" ? null : activeType,
          motionFormat: activeType === "motion" && activeMotionFormat !== "all" ? activeMotionFormat : null,
          query: searchText || null,
          rootId: activeRoot,
          directoryPath: requestDirectory,
          recursiveScope,
          favoritesOnly,
          filterId: activeSavedFilterId,
          expression: quickExpression,
          cursor,
          limit: LIBRARY_PAGE_SIZE,
        });
      if (revision !== assetQueryRevision.current) return;
      setAssets((current) => {
        const existingIds = new Set(current.map((asset) => asset.id));
        return [...current, ...page.items.filter((asset) => !existingIds.has(asset.id))];
      });
      setNextAssetCursor(page.nextCursor);
      if (pendingScrollRestore.current) pendingScrollRestore.current = { ...pendingScrollRestore.current, retryBlocked: false };
    } catch (reason) {
      if (revision === assetQueryRevision.current) {
        setError(toUiError(reason));
        if (pendingScrollRestore.current) pendingScrollRestore.current = { ...pendingScrollRestore.current, retryBlocked: true };
      }
    } finally {
      if (assetPageLoading.current === revision) assetPageLoading.current = null;
      if (revision === assetQueryRevision.current) setLoadingNextPage(false);
    }
  }, [activeRoot, activeDirectory, activeSavedFilterId, activeType, activeMotionFormat, favoritesOnly, nextAssetCursor, searchText, recursiveScope, viewMode, quickExpression]);

  useEffect(() => {
    if (!nextAssetCursor || !libraryScrollParent || !loadMoreRef.current || !("IntersectionObserver" in window)) return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) void loadNextAssetPage();
    }, { root: libraryScrollParent, rootMargin: "400px" });
    observer.observe(loadMoreRef.current);
    return () => observer.disconnect();
  }, [nextAssetCursor, libraryScrollParent, loadNextAssetPage]);

  useEffect(() => {
    const pending = pendingScrollRestore.current;
    if (viewMode !== "assets" || isRefreshing || !pending || pending.retryBlocked || assetPageRevision < pending.afterRevision || !libraryScrollParent) return;
    const maxTop = Math.max(0, libraryScrollParent.scrollHeight - libraryScrollParent.clientHeight);
    if (maxTop + 2 < pending.top && nextAssetCursor) {
      if (!loadingNextPage) void loadNextAssetPage();
      return;
    }
    let secondFrame = 0;
    const firstFrame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(() => {
        if (pendingScrollRestore.current !== pending) return;
        const availableTop = Math.max(0, libraryScrollParent.scrollHeight - libraryScrollParent.clientHeight);
        const targetTop = Math.min(pending.top, availableTop);
        libraryScrollParent.scrollTo({ top: targetTop });
        if (Math.abs(libraryScrollParent.scrollTop - targetTop) <= 2 || !nextAssetCursor) {
          pendingScrollRestore.current = null;
        } else if (!loadingNextPage) {
          void loadNextAssetPage();
        }
      });
    });
    return () => {
      cancelAnimationFrame(firstFrame);
      if (secondFrame) cancelAnimationFrame(secondFrame);
    };
  }, [viewMode, isRefreshing, libraryScrollParent, assetPageRevision, assets.length, nextAssetCursor, loadingNextPage, loadNextAssetPage]);

  const refreshAsset = useCallback(async (assetId: string) => {
    const asset = await invoke<Asset>("asset_inspect", { assetId });
    setAssets((items) => items.map((item) => item.id === assetId ? asset : item));
    setSelected((item) => item?.id === assetId ? asset : item);
    setPreviewAsset((item) => item?.id === assetId ? asset : item);
    setThumbnailRevisions((current) => ({ ...current, [assetId]: (current[assetId] ?? 0) + 1 }));
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);

  useEffect(() => {
    let disposed = false;
    let unlisten: Array<() => void> = [];
    void Promise.all([
      listen<string>("library-changed", () => { if (!disposed) void refresh(); }),
      listen<{ rootId: string; message: string }>("library-watch-error", (event) => {
        if (!disposed) setError(`目录监视/自动扫描失败（${event.payload.rootId}）：${event.payload.message}`);
      }),
    ])
      .then((listeners) => {
        if (disposed) listeners.forEach((stop) => stop());
        else unlisten = listeners;
      })
      .catch((reason) => { if (!disposed) setError(toUiError(reason)); });
    return () => {
      disposed = true;
      unlisten.forEach((stop) => stop());
    };
  }, [refresh]);

  useEffect(() => {
    const active = jobs.some((job) => activeThumbnailStatuses.has(job.status));
    if (!active) return;
    let disposed = false;
    const timer = window.setInterval(() => {
      void Promise.all([invoke<Job[]>("jobs_list"), invoke<JobSummary>("jobs_summary")])
        .then(([nextJobs, nextSummary]) => {
          if (disposed) return;
          setJobs(nextJobs);
          setJobSummary(nextSummary);
          if (batchRegeneration.current && !(nextSummary.Pending || nextSummary.Parsing || nextSummary.Rendering || nextSummary.Encoding || nextSummary.Cancelling)) {
            batchRegeneration.current = false;
            void refresh();
          }
          for (const job of nextJobs) {
            if (job.status === "Completed" && jobs.some((previous) => previous.id === job.id && activeThumbnailStatuses.has(previous.status)) && job.asset_id) {
              void refreshAsset(job.asset_id).catch((reason) => setError(toUiError(reason)));
            }
          }
        })
        .catch((reason) => { if (!disposed) setError(toUiError(reason)); });
    }, 700);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [jobs, refreshAsset, refresh]);

  useEffect(() => {
    let disposed = false;
    const timer = window.setInterval(() => {
      void invoke<ScanState[]>("scan_states").then((nextScans) => {
        if (disposed) return;
        const previous = scanStatesRef.current;
        scanStatesRef.current = nextScans;
        setScanStates(nextScans);
        if (nextScans.some((scan) => activeScanStatuses.has(scan.status))) {
          void invoke<AssetCounts>("asset_counts").then((nextCounts) => { if (!disposed) setCounts(nextCounts); }).catch(() => {});
        }
        if (previous.some((scan) => activeScanStatuses.has(scan.status)
          && ["Completed", "Paused", "Cancelled", "Failed"].includes(nextScans.find((next) => next.rootId === scan.rootId)?.status ?? ""))) {
          void refresh();
        }
      }).catch((reason) => { if (!disposed) setError(toUiError(reason)); });
    }, 1200);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [refresh]);

  useEffect(() => {
    if (!selected) {
      setSelectedDetails(null);
      return;
    }
    const assetId = selected.id;
    const revision = selectedDetailsRevision;
    setSelectedDetails({ assetId, revision, status: "loading", tags: [], relations: [], error: "" });
    let active = true;
    Promise.all([
      invoke<Asset>("asset_inspect", { assetId }),
      invoke<AssetTag[]>("asset_tags", { assetId }),
      invoke<AssetRelation[]>("relations_list", { assetId, limit: 100 }),
    ])
      .then(([details, tags, relations]) => {
        if (active && selectedDetailsRef.current.assetId === assetId && selectedDetailsRef.current.revision === revision) {
          setSelected((current) => current?.id === details.id ? details : current);
          setSelectedDetails({ assetId, revision, status: "ready", tags, relations, error: "" });
        }
      })
      .catch((reason) => {
        if (active && selectedDetailsRef.current.assetId === assetId && selectedDetailsRef.current.revision === revision) {
          setSelectedDetails({ assetId, revision, status: "failed", tags: [], relations: [], error: toUiError(reason) });
        }
      });
    return () => { active = false; };
  }, [selected?.id, selectedDetailsRevision]);

  const activeJobs = useMemo(
    () => jobs.filter((job) => activeThumbnailStatuses.has(job.status)),
    [jobs],
  );
  const activeScans = useMemo(() => scanStates.filter((scan) => activeScanStatuses.has(scan.status)), [scanStates]);
  const activeThumbnailCount = (jobSummary.Pending ?? 0) + (jobSummary.Parsing ?? 0)
    + (jobSummary.Rendering ?? 0) + (jobSummary.Encoding ?? 0) + (jobSummary.Cancelling ?? 0);
  const totalThumbnailCount = Object.values(jobSummary).reduce((total, count) => total + count, 0);

  function selectCategory(type: AssetType | "all", stayInFolders = false) {
    setFavoritesOnly(false);
    setActiveSavedFilterId(null);
    setActiveType(type);
    if (!stayInFolders) setViewMode("assets");
    setActiveRoot(null);
    setActiveDirectory(null);
    setSelected(null);
  }

  function setFolderBrowserCollapse(progress: number) {
    libraryScrollParent?.style.setProperty("--folder-collapse", String(progress));
    setFolderBrowserCompact(progress >= 1);
  }

  function enterFolderView(asset?: Asset) {
    pendingScrollRestore.current = null;
    setFolderBrowserCollapse(Math.min(1, Math.max(0, (libraryScrollParent?.scrollTop ?? 0) / 240)));
    if (viewMode === "assets") {
      const snapshot: AssetViewState = {
        activeType, activeMotionFormat, activeRoot, activeDirectory, activeSavedFilterId,
        favoritesOnly, query, searchText, scrollTop: libraryScrollParent?.scrollTop ?? 0,
        selectedTags, tagMatch, skeletonClass, recursiveScope,
      };
      folderReturnState.current = snapshot;
      writePreference("mmdbridge-asset-view-return", JSON.stringify(snapshot));
    }
    setViewMode("folders");
    setFavoritesOnly(false);
    setActiveSavedFilterId(null);
    setQuery("");
    setSearchText("");
    setSelectedTags([]);
    setSkeletonClass("all");
    setSelected(null);
    setBulkSelectMode(false);
    setBulkSelectedIds(new Set());
    if (asset) {
      setActiveRoot(asset.rootId);
      setActiveDirectory(asset.assetDirectory);
      setActiveType(asset.assetType);
      setActiveMotionFormat("all");
      return;
    }
    if (!activeRoot) {
      const savedRoot = readPreference("mmdbridge-folder-root");
      const savedPath = readPreference("mmdbridge-folder-path");
      const root = rootsRef.current.find((item) => item.id === savedRoot);
      if (root) {
        setActiveRoot(root.id);
        setActiveType(root.assetType);
        setActiveDirectory(savedPath && isPathWithinRoot(savedPath, root.path) && !isPathWithinRoot(root.path, savedPath)
          ? savedPath
          : null);
      } else {
        setActiveRoot(null);
        setActiveDirectory(null);
      }
    }
  }

  function returnToAssetView() {
    const saved = folderReturnState.current ?? readAssetViewState("mmdbridge-asset-view-return");
    pendingScrollRestore.current = { top: saved?.scrollTop ?? 0, afterRevision: assetQueryRevision.current + 1, retryBlocked: false };
    setViewMode("assets");
    setSelected(null);
    if (!saved) return;
    setActiveType(saved.activeType);
    setActiveMotionFormat(saved.activeMotionFormat);
    setActiveRoot(saved.activeRoot);
    setActiveDirectory(saved.activeDirectory);
    setActiveSavedFilterId(saved.activeSavedFilterId);
    setFavoritesOnly(saved.favoritesOnly);
    setQuery(saved.query);
    setSearchText(saved.searchText);
    setSelectedTags(saved.selectedTags);
    setTagMatch(saved.tagMatch);
    setSkeletonClass(saved.skeletonClass);
    setRecursiveScope(saved.recursiveScope);
  }

  function viewAssetDirectory(asset: Asset) {
    enterFolderView(asset);
  }

  function selectMotionFormat(format: MotionFormat) {
    setActiveMotionFormat(format);
    setAssets([]);
    setSelected(null);
    setNextAssetCursor(null);
  }

  function addRoot(type?: AssetType) {
    setAddRootType(type ?? (activeType === "all" ? "model" : activeType));
    setAddRootPath("");
    setAddRootName("");
    setAddRootRecursive(true);
    setError("");
    setNotice("");
    setAddRootOpen(true);
  }

  async function chooseAddRootDirectory() {
    try {
      const path = await open({ directory: true, multiple: false, title: `选择${categoryLabels[addRootType]}目录` });
      if (typeof path === "string" && path.trim()) setAddRootPath(path.trim());
    } catch (reason) {
      setError(toUiError(reason));
    }
  }

  async function submitAddRoot() {
    const path = addRootPath.trim();
    const name = addRootName.trim();
    if (!path) {
      setError("请选择或输入目录路径。");
      return;
    }
    if (Array.from(name).length > 128) {
      setError("显示名称不能超过 128 个字符。");
      return;
    }
    setAddRootBusy(true);
    setNotice("正在添加目录并加入扫描队列…");
    try {
      const root = await invoke<Root>("root_add", {
        assetType: addRootType,
        path,
        name: name || null,
        scanRecursive: addRootRecursive,
      });
      setRoots((current) => [...current.filter((item) => item.id !== root.id), root]);
      setActiveType(addRootType);
      setActiveRoot(root.id);
      setActiveSavedFilterId(null);
      setFavoritesOnly(false);
      try {
        const scan = await invoke<ScanState>("scan_enqueue", { rootId: root.id });
        setScanStates((current) => [...current.filter((item) => item.rootId !== root.id), scan]);
        scanStatesRef.current = [...scanStatesRef.current.filter((item) => item.rootId !== root.id), scan];
        setError("");
        setNotice(`${root.displayName} 已添加并加入扫描队列。`);
      } catch (reason) {
        setError(`目录已添加，但扫描未能启动：${toUiError(reason)}`);
        setNotice("目录仍保留在列表中；请点击目录旁的扫描按钮重试。");
      }
      setAddRootOpen(false);
      void refresh();
    } catch (reason) {
      setError(toUiError(reason));
      setNotice("");
    } finally {
      setAddRootBusy(false);
    }
  }

  async function openModelPreview() {
    const path = await open({
      multiple: false,
      title: "打开模型预览",
      filters: [{ name: "PMX / PMD 模型", extensions: ["pmx", "pmd"] }],
    });
    if (typeof path !== "string" || !path.trim()) return;
    const name = path.split(/[\\/]/).pop() ?? path;
    setViewerAsset({ name, primarySource: path });
  }

  async function openSettings() {
    setSettingsError("");
    try {
      const [path, concurrency, storage] = await Promise.all([
        invoke<string | null>("motion_preview_model_get"),
        invoke<ThumbnailConcurrencySettings>("thumbnail_concurrency_get"),
        invoke<StorageInfo>("storage_info"),
      ]);
      setMotionPreviewModel(path);
      setThumbnailConcurrencyDraft(concurrency);
      setStorageInfo(storage);
      setSettingsOpen(true);
    } catch (reason) {
      setSettingsError(toUiError(reason));
      setError(toUiError(reason));
    }
  }

  async function chooseMotionPreviewModel() {
    const path = await open({
      multiple: false,
      title: "选择 Motion Preview Model",
      filters: [{ name: "PMX / PMD 模型", extensions: ["pmx", "pmd"] }],
    });
    if (typeof path !== "string" || !path.trim()) return;
    setSettingsBusy(true);
    setSettingsError("");
    try {
      const saved = await invoke<string | null>("motion_preview_model_set", { path });
      setMotionPreviewModel(saved);
      setNotice("已保存动作预览模型。VMD/VPD 资源卡会使用该模型生成动作或姿势缩略图。");
      setError("");
    } catch (reason) {
      setSettingsError(toUiError(reason));
      setError(toUiError(reason));
    } finally {
      setSettingsBusy(false);
    }
  }

  async function clearMotionPreviewModel() {
    setSettingsBusy(true);
    setSettingsError("");
    try {
      await invoke<string | null>("motion_preview_model_set", { path: null });
      setMotionPreviewModel(null);
      setNotice("已清除动作预览模型设置。");
      setError("");
    } catch (reason) {
      setSettingsError(toUiError(reason));
      setError(toUiError(reason));
    } finally {
      setSettingsBusy(false);
    }
  }

  async function saveThumbnailConcurrency() {
    setSettingsBusy(true);
    setSettingsError("");
    try {
      const saved = await invoke<ThumbnailConcurrencySettings>("thumbnail_concurrency_set", { settings: thumbnailConcurrencyDraft });
      setThumbnailConcurrencyDraft(saved);
      setNotice("已保存缩略图并发设置，新设置立即生效。");
      setError("");
    } catch (reason) {
      setSettingsError(toUiError(reason));
      setError(toUiError(reason));
    } finally {
      setSettingsBusy(false);
    }
  }

  async function compactStorage() {
    setSettingsBusy(true);
    setSettingsError("");
    try {
      const storage = await invoke<StorageInfo>("storage_compact");
      setStorageInfo(storage);
      setNotice("数据库整理完成，已保留最近 1000 条任务记录。");
      setError("");
    } catch (reason) { setSettingsError(toUiError(reason)); setError(toUiError(reason)); }
    finally { setSettingsBusy(false); }
  }

  async function requestAssetOperation(
    operation: AssetOperationPlan["operation"],
    assetIds: string[],
    destinationParent: string | null = null,
    newName: string | null = null,
  ) {
    setBusy(true);
    setAssetOperationError("");
    try {
      const plan = await invoke<AssetOperationPlan>("asset_operation_plan", {
        operation,
        assetIds,
        destinationParent,
        newName,
      });
      setAssetOperationPlan(plan);
      setError("");
      setNotice("");
    } catch (reason) {
      setError(toUiError(reason));
    } finally {
      setBusy(false);
    }
  }

  async function planMoveAssets(assetIds: string[]) {
    const destination = await open({ directory: true, multiple: false, title: "选择资产包移动目标目录" });
    if (typeof destination !== "string" || !destination.trim()) return;
    await requestAssetOperation("move", assetIds, destination.trim());
  }

  async function planRenameAsset(asset: Asset) {
    const currentName = asset.assetDirectory.split(/[\\/]/).filter(Boolean).pop() ?? asset.name;
    const newName = await dialogs.prompt("重命名整个资产包文件夹", currentName);
    if (!newName?.trim()) return;
    await requestAssetOperation("rename", [asset.id], null, newName.trim());
  }

  async function openOperationJournal() {
    setOperationJournalError("");
    try {
      const entries = await invoke<AssetOperationJournalEntry[]>("operation_journal_list", { limit: 100 });
      setOperationJournal(entries);
      setOperationJournalOpen(true);
      setError("");
    } catch (reason) {
      setOperationJournalError(toUiError(reason));
      setError(toUiError(reason));
    }
  }

  async function resolveJournalEntry(entry: AssetOperationJournalEntry) {
    if (!await dialogs.confirm("请先在资源管理器检查源路径与目标路径，并恢复文件或重新扫描相关资产。确认已完成这些人工核对后，才标记此记录已处理。")) return;
    setOperationJournalError("");
    try {
      await invoke<boolean>("operation_journal_resolve", { operationId: entry.id });
      setOperationJournal(await invoke<AssetOperationJournalEntry[]>("operation_journal_list", { limit: 100 }));
      await refresh();
      setNotice("恢复记录已标记为已核对。");
      setError("");
    } catch (reason) { setOperationJournalError(toUiError(reason)); setError(toUiError(reason)); }
  }

  async function executePlannedAssetOperation() {
    if (!assetOperationPlan?.canExecute || busy) return;
    setBusy(true);
    setAssetOperationError("");
    try {
      const entry = await invoke<AssetOperationJournalEntry>("asset_operation_execute", { plan: assetOperationPlan });
      setAssetOperationPlan(null);
      setSelected(null);
      setBulkSelectedIds(new Set());
      setBulkSelectMode(false);
      setNotice(entry.result?.message ?? "资产操作已完成。");
      setError("");
      await refresh();
      if (operationJournalOpen) {
        try { setOperationJournal(await invoke<AssetOperationJournalEntry[]>("operation_journal_list", { limit: 100 })); }
        catch (reason) { setOperationJournalError(toUiError(reason)); setError(toUiError(reason)); }
      }
    } catch (reason) {
      setAssetOperationError(toUiError(reason));
      setError(toUiError(reason));
    } finally {
      setBusy(false);
    }
  }

  async function revealAsset(asset: Asset) {
    try {
      await invoke("asset_reveal", { assetId: asset.id });
      setNotice("已在资源管理器中定位源文件。");
      setError("");
    } catch (reason) {
      setError(toUiError(reason));
      setNotice("");
    }
  }

  async function openAssetDirectory(asset: Asset) {
    try {
      await invoke("asset_open_directory", { assetId: asset.id });
      setNotice("已打开资产源目录。");
      setError("");
    } catch (reason) {
      setError(toUiError(reason));
      setNotice("");
    }
  }

  function addAnyRoot() {
    addRoot();
  }

  async function scanRoot(root: Root, fullCheck = false) {
    try {
      const paused = scanStatesRef.current.some((item) => item.rootId === root.id && item.status === "Paused");
      if (fullCheck && paused) {
        setError("请先继续或停止已暂停的扫描，再开始完整检查。");
        return;
      }
      const state = await invoke<ScanState>(paused ? "scan_continue" : fullCheck ? "scan_full_check" : "scan_enqueue", { rootId: root.id });
      setScanStates((current) => [...current.filter((item) => item.rootId !== root.id), state]);
      scanStatesRef.current = [...scanStatesRef.current.filter((item) => item.rootId !== root.id), state];
      setNotice(`${root.displayName} 已加入${fullCheck ? "完整检查" : "扫描"}队列。`);
    } catch (reason) {
      setError(toUiError(reason));
      setNotice("");
    }
  }

  async function cancelScan(rootId: string) {
    try {
      await invoke("scan_cancel", { rootId });
      const states = await invoke<ScanState[]>("scan_states");
      scanStatesRef.current = states;
      setScanStates(states);
      setNotice("已请求停止扫描。已建立的索引记录会保留。");
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function pauseScan(rootId: string) {
    try {
      await invoke("scan_pause", { rootId });
      const states = await invoke<ScanState[]>("scan_states");
      scanStatesRef.current = states;
      setScanStates(states);
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function moveScan(rootId: string, direction: -1 | 1) {
    try {
      await invoke("scan_move", { rootId, direction });
      const states = await invoke<ScanState[]>("scan_states");
      scanStatesRef.current = states;
      setScanStates(states);
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function queueCards(root: Root) {
    setNotice(`正在为 ${root.displayName} 安排软件生成资源卡…`);
    try {
      const count = await invoke<number>("cards_queue_root", { rootId: root.id });
      await refresh();
      setNotice(`${root.displayName} 已加入 ${count} 个资源卡生成任务。`);
    } catch (reason) { setError(toUiError(reason)); setNotice(""); }
  }

  async function updateRoot(root: Root, changes: { enabled?: boolean; scanRecursive?: boolean; displayName?: string }) {
    try {
      const updated = await invoke<Root>("root_update", {
        rootId: root.id,
        enabled: changes.enabled ?? null,
        scanRecursive: changes.scanRecursive ?? null,
        displayName: changes.displayName ?? null,
      });
      setRoots((current) => current.map((item) => item.id === updated.id ? updated : item));
      await refresh();
      setNotice(changes.displayName !== undefined ? "已更新目录名称。" : changes.enabled !== undefined ? (updated.enabled ? "已启用目录监视与后续扫描。" : "已停用目录监视与后续扫描。") : updated.scanRecursive ? "已开启递归扫描；下次扫描会检查目录及其子目录。" : "已关闭递归扫描；下次扫描仅检查顶层文件，子目录已有索引状态会保留。");
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function renameRoot(root: Root) {
    const displayName = await dialogs.prompt("目录显示名称", root.displayName);
    if (displayName === null || displayName.trim() === "" || displayName.trim() === root.displayName) return;
    void updateRoot(root, { displayName: displayName.trim() });
  }

  async function removeRoot(root: Root) {
    if (!await dialogs.confirm(`从资产库移除“${root.displayName}”？磁盘文件不会删除。`)) return;
    try {
      await invoke("root_remove", { rootId: root.id });
      setActiveRoot(null);
      setSelected(null);
      await refresh();
      setNotice("已从资产库移除目录索引。");
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function createCard(assetId: string) {
    setBusy(true);
    try {
      await invoke("thumbnail_regenerate", { assetId });
      setJobs(await invoke<Job[]>("jobs_list"));
      setJobSummary(await invoke<JobSummary>("jobs_summary"));
      await refreshAsset(assetId);
      setNotice("该资产的缩略图已加入重生成队列。");
    } catch (reason) { setError(toUiError(reason)); }
    finally { setBusy(false); }
  }

  async function regenerateAllThumbnails() {
    setSettingsBusy(true);
    setSettingsError("");
    try {
      const result = await invoke<{ queued: number; skipped: number; failed: number }>("thumbnails_regenerate_all");
      batchRegeneration.current = result.queued > 0;
      void refresh();
      setJobs(await invoke<Job[]>("jobs_list"));
      setJobSummary(await invoke<JobSummary>("jobs_summary"));
      setNotice(`重生成已入队 ${result.queued} 项，跳过 ${result.skipped} 项，入队失败 ${result.failed} 项。可在后台任务中取消或重试。`);
    } catch (reason) { setSettingsError(toUiError(reason)); setError(toUiError(reason)); }
    finally { setSettingsBusy(false); }
  }

  async function cancelJob(jobId: string) {
    try {
      const changed = await invoke<boolean>("jobs_cancel", { jobId });
      await refresh();
      setNotice(changed ? "已请求取消缩略图任务，正在等待工作线程退出。" : "任务已结束或正在取消。");
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function retryJob(jobId: string) {
    try {
      await invoke("jobs_retry", { jobId });
      await refresh();
      setNotice("已重新加入缩略图队列。");
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function verifyCard(assetId: string) {
    try {
      const result = await invoke<{ status: string; message?: string }>("card_verify", { assetId });
      await refresh();
      setNotice(result.message ?? `资源卡校验结果：${result.status}`);
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function addTag() {
    if (!selected || !detailsReady || busy) return;
    const assetId = selected.id;
    const revision = selectedDetailsRevision;
    const name = await dialogs.prompt("添加标签");
    if (!name?.trim() || !isCurrentDetails(assetId, revision)) return;
    try {
      const result = await invoke<{ blockedByUser: boolean }>("tag_add", { assetId, name: name.trim(), source: "user" });
      await refresh();
      setNotice(result.blockedByUser ? "此标签已被手动移除，自动标签来源不会重新添加它。" : "已保存用户标签。");
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function addTagToSelection() {
    const assetIds = Array.from(bulkSelectedIds);
    if (!assetIds.length) return;
    const name = await dialogs.prompt(`为 ${assetIds.length} 项资产添加用户标签`);
    if (!name?.trim()) return;

    setBusy(true);
    try {
      const mutations = await invoke<Array<{ changed: boolean; blockedByUser: boolean }>>("tag_add_batch", {
        assetIds,
        name: name.trim(),
        source: "user",
      });
      const changed = mutations.filter((mutation) => mutation.changed).length;
      const blocked = mutations.filter((mutation) => mutation.blockedByUser).length;
      setBulkSelectedIds(new Set());
      setBulkSelectMode(false);
      setNotice(`批量标签已完成：更新 ${changed}/${mutations.length} 项，受手动移除规则阻止 ${blocked} 项。`);
      await refresh();
    } catch (reason) {
      setError(`批量标签失败：${toUiError(reason)}`);
    } finally {
      setBusy(false);
    }
  }

  async function removeTagFromSelection() {
    const assetIds = Array.from(bulkSelectedIds);
    if (!assetIds.length) return;
    const name = await dialogs.prompt(`从 ${assetIds.length} 项资产移除哪个标签？手动移除会阻止后续自动标签重新添加。`);
    if (!name?.trim()) return;

    setBusy(true);
    try {
      const mutations = await invoke<Array<{ changed: boolean }>>("tag_remove_batch", {
        assetIds,
        name: name.trim(),
      });
      const changed = mutations.filter((mutation) => mutation.changed).length;
      setBulkSelectedIds(new Set());
      setBulkSelectMode(false);
      setNotice(`批量移除标签完成：移除 ${changed}/${mutations.length} 项，已为所选资产记录手动移除规则。`);
      await refresh();
    } catch (reason) {
      setError(`批量移除标签失败：${toUiError(reason)}`);
    } finally {
      setBusy(false);
    }
  }

  async function setFavoriteForSelection(favorite: boolean) {
    const assetIds = Array.from(bulkSelectedIds);
    if (!assetIds.length) return;
    setBusy(true);
    try {
      const changed = await invoke<number>("favorite_set_batch", { assetIds, favorite });
      setBulkSelectedIds(new Set());
      setBulkSelectMode(false);
      setNotice(`${favorite ? "批量收藏" : "批量取消收藏"}已完成：${changed}/${assetIds.length} 项状态发生变化。`);
      await refresh();
    } catch (reason) {
      setError(`批量${favorite ? "收藏" : "取消收藏"}失败：${toUiError(reason)}`);
    } finally {
      setBusy(false);
    }
  }

  function toggleBulkSelection(asset: Asset) {
    if (!bulkSelectMode) {
      setSelected(asset);
      // Keep card positions stable between the two clicks of a double-click.
      // The details button controls the panel; an open panel follows selection.
      return;
    }
    setBulkSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(asset.id)) next.delete(asset.id);
      else next.add(asset.id);
      return next;
    });
  }

  function openAsset3D(asset: Asset) {
    if (asset.assetType === "motion") {
      if (asset.primarySource.toLowerCase().endsWith(".vmd")) setMotionViewerAsset(asset);
      else setPreviewAsset(asset);
      return;
    }
    setViewerAsset({ id: asset.id, name: asset.name, primarySource: asset.primarySource, assetType: asset.assetType });
  }

  async function removeTag(name: string) {
    if (!selected || !detailsReady || busy) return;
    const assetId = selected.id;
    try {
      await invoke("tag_remove", { assetId, name });
      await refresh();
      setNotice(`已移除标签“${name}”。`);
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function toggleFavorite(asset: Asset) {
    try {
      await invoke("favorite_set", { assetId: asset.id, favorite: !asset.isFavorite });
      await refresh();
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function confirmRelation(relation: AssetRelation): Promise<boolean> {
    if (!selected || !detailsReady || busy || !assetRelations.some((item) => item.id === relation.id)) return false;
    const assetId = selected.id;
    const revision = selectedDetailsRevision;
    try {
      const confirmed = await invoke<boolean>("relation_confirm", { relationId: relation.id });
      if (!isCurrentDetails(assetId, revision)) return false;
      if (!confirmed) {
        setSelectedDetailsRevision((value) => value + 1);
        setError("这条关系建议已失效，已刷新列表；请重新选择。");
        return false;
      }
      setSelectedDetails((current) => current?.assetId === assetId && current.revision === revision ? { ...current, relations: current.relations.map((item) => {
        if (relation.relationType === "MotionCameraPair" && item.relationType === "MotionCameraPair" && item.sourceAsset === relation.sourceAsset) {
          return { ...item, confirmed: item.id === relation.id };
        }
        return item.id === relation.id ? { ...item, confirmed: true } : item;
      }) } : current);
      setNotice("已确认这条关系建议。");
      return true;
    } catch (reason) { setError(toUiError(reason)); return false; }
  }

  async function selectCameraAndPreview(relation: AssetRelation, motion: Asset) {
    if (await confirmRelation(relation)) setMotionViewerAsset(motion);
  }

  function isCurrentDetails(assetId: string, revision: number): boolean {
    const current = selectedDetailsRef.current;
    return current.assetId === assetId && current.revision === revision && current.ready;
  }

  function changeFilterRule(index: number, updates: Partial<BuilderRule>) {
    setFilterRules((current) => current.map((rule, ruleIndex) => ruleIndex === index ? { ...rule, ...updates } : rule));
  }

  function changeFilterField(index: number, field: FilterField) {
    const value = booleanFilterFields.has(field) ? "true" : numericFilterFields.has(field) ? "0" : field === "assetType" ? "motion" : field === "cardStatus" ? "CardValid" : "";
    if (field === "skeletonClass") { changeFilterRule(index, { field, operator: "eq", value: "nonstandard", negate: false }); return; }
    const operator: FilterOperator = numericFilterFields.has(field) || dateFilterFields.has(field) ? "gte" : ["tag", "directory"].includes(field) ? "contains" : "eq";
    changeFilterRule(index, { field, operator, value });
  }

  function selectSavedFilter(filter: SavedFilter) {
    if (hasRetiredFilterCondition(filter.expression)) {
      setError(`智能集合“${filter.name}”包含已停用的条件，未应用。原条件已保留；请新建替代智能集合。`);
      return;
    }
    setFavoritesOnly(false);
    setActiveSavedFilterId(filter.id);
    setActiveType("all");
    setViewMode("assets");
    setActiveRoot(null);
    setActiveDirectory(null);
    setSelected(null);
    setFilterBuilderOpen(false);
    setQuery("");
    setSearchText("");
    setError("");
  }

  async function saveSmartFilter() {
    if (!filterName.trim()) {
      setError("请为智能集合输入名称。");
      return;
    }
    try {
      const children: FilterExpr[] = filterRules.map((rule) => {
        if (rule.field === "fileType" && rule.value.trim().replace(/^\.+/, "").toLowerCase() === "x") {
          throw new Error("X 格式已停用，不能再用它创建文件格式条件。");
        }
        if (!booleanFilterFields.has(rule.field) && !rule.value.trim()) {
          throw new Error(`${filterFieldLabels[rule.field]}需要填写匹配值。`);
        }
        let value: string | number | boolean = rule.value;
        if (booleanFilterFields.has(rule.field)) value = rule.value === "true";
        else if (numericFilterFields.has(rule.field)) {
          const number = Number(rule.value);
          if (!Number.isFinite(number)) throw new Error(`${filterFieldLabels[rule.field]}需要有效数字。`);
          value = number;
        } else if (dateFilterFields.has(rule.field)) {
          const date = new Date(rule.value);
          if (!Number.isFinite(date.getTime())) throw new Error(`${filterFieldLabels[rule.field]}需要有效日期。`);
          value = date.toISOString();
        }
        const expression: FilterExpr = { op: "rule", field: rule.field, operator: rule.operator, value };
        return rule.negate ? { op: "not", child: expression } : expression;
      });
      const saved = await invoke<SavedFilter>("filter_save", {
        filterId: null,
        name: filterName.trim(),
        expression: { op: filterGroupOp, children },
      });
      setFilterName("");
      setFavoritesOnly(false);
      setActiveType("all");
      setActiveRoot(null);
      setSelected(null);
      setActiveSavedFilterId(saved.id);
      setViewMode("assets");
      setActiveDirectory(null);
      setFilterBuilderOpen(false);
      setQuery("");
      setSearchText("");
      setNotice(`已保存并应用智能集合“${saved.name}”。`);
      setError("");
    } catch (reason) { setError(toUiError(reason)); }
  }

  async function removeSmartFilter(filter: SavedFilter) {
    if (!await dialogs.confirm(`删除智能集合“${filter.name}”？资产文件不会受到影响。`)) return;
    try {
      await invoke("filter_remove", { filterId: filter.id });
      if (activeSavedFilterId === filter.id) setActiveSavedFilterId(null);
      else await refresh();
    } catch (reason) { setError(toUiError(reason)); }
  }

  function submitSearch(event: React.FormEvent) {
    event.preventDefault();
    setSearchText(query.trim());
  }

  const visibleAssets = assets.filter((asset) => (viewMode !== "folders" || !!activeRoot)
    && (activeType === "all" || asset.assetType === activeType)
    && (!activeRoot || asset.rootId === activeRoot));
  const folderRoot = roots.find((root) => root.id === activeRoot);
  const currentDirectoryPage = folderRoot && directoryPage
    && sameDirectoryPath(directoryPage.path, activeDirectory ?? folderRoot.path) ? directoryPage : null;
  const folderTree = useMemo(() => folderRoot ? buildFolderTree(folderRoot.path, assetDirectories) : [], [folderRoot?.path, assetDirectories]);
  const cachedDirectoryCount = folderRoot ? assetDirectories.reduce((total, directory) => {
    const selectedPath = activeDirectory ?? folderRoot.path;
    return total + ((recursiveScope ? isPathWithinRoot(directory.path, selectedPath) : sameDirectoryPath(directory.path, selectedPath)) ? directory.count : 0);
  }, 0) : 0;
  function renderFolderNodes(nodes: FolderTreeNode[], depth = 0): React.ReactNode {
    return nodes.slice().sort((left, right) => left.name.localeCompare(right.name, "zh-CN")).map((node) => {
      const selectedFolder = activeDirectory !== null && sameDirectoryPath(activeDirectory, node.path);
      const expanded = activeDirectory !== null && isPathWithinRoot(activeDirectory, node.path);
      return <div key={node.path}>
        <UnstyledButton className={`folder-tree-node ${selectedFolder ? "selected" : ""}`} style={{ paddingLeft: `${10 + depth * 16}px` }} title={node.path} aria-expanded={node.children.size ? expanded : undefined} aria-current={selectedFolder ? "location" : undefined} onClick={() => { setActiveDirectory(node.path); setSelected(null); }}>
          <span aria-hidden="true">{node.children.size ? expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} /> : "·"}</span><strong>{node.name}</strong><small>{node.count.toLocaleString()}</small>
        </UnstyledButton>
        {expanded && node.children.size > 0 && renderFolderNodes(Array.from(node.children.values()), depth + 1)}
      </div>;
    });
  }
  const indexedTotal = viewMode === "folders" && activeRoot
    ? folderRoot && (activeType === "all" || activeType === folderRoot.assetType)
      ? currentDirectoryPage?.visibleCount ?? (cachedDirectoryCount || (recursiveScope && !activeDirectory ? counts.byRoot[activeRoot] ?? 0 : 0)) : 0
    : activeDirectory ? directoryPage?.visibleCount ?? 0
      : activeRoot ? counts.byRoot[activeRoot] ?? 0 : activeType === "all" ? counts.all : counts[activeType];
  const showIndexedTotal = !searchText && !favoritesOnly && !activeSavedFilterId && !quickExpression && (activeType !== "motion" || activeMotionFormat === "all") && (viewMode !== "folders" || !!activeRoot);
  const activeTitle = viewMode === "folders"
    ? folderRoot?.displayName ?? "文件夹"
    : favoritesOnly ? "收藏" : savedFilters.find((filter) => filter.id === activeSavedFilterId)?.name ?? (activeRoot ? roots.find((root) => root.id === activeRoot)?.displayName ?? "资产库" : activeType === "all" ? "全部资产" : categoryLabels[activeType]);
  const libraryHomeActive = viewMode === "assets" && activeType === "all" && !activeRoot && !favoritesOnly && !activeSavedFilterId;
  const hasQueryFilters = !!(searchText || quickExpression || activeSavedFilterId || (activeType === "motion" && activeMotionFormat !== "all"));
  const emptyKind = assetQueryError ? "error" : !roots.length ? "no-roots" : hasQueryFilters ? "filter" : favoritesOnly ? "favorite"
    : (activeDirectory || (viewMode === "folders" && activeRoot)) && (activeRoot ? (counts.byRoot[activeRoot] ?? 0) > 0 : counts.all > 0) ? "folder" : "collection";
  const emptyCopy = {
    error: { title: "资产读取失败", description: assetQueryError },
    "no-roots": { title: "尚未添加资产目录", description: "添加模型、动作或场景目录后，扫描其中的资产。" },
    filter: { title: "没有符合当前条件的资产", description: "试试其他条件，或清除搜索、标签、骨架、动作格式和智能集合条件。当前文件夹范围会保留。" },
    favorite: { title: "还没有收藏资产", description: "返回资产库，在资产卡片菜单或详情中添加收藏。" },
    folder: { title: "当前文件夹没有已索引资产", description: recursiveScope ? "此文件夹及子目录中没有符合当前类型的资产。可返回资产库，或查看扫描进度。" : "当前只查看此文件夹。可勾选“包含子目录”，或返回资产库。" },
    collection: { title: "这个集合还没有已索引资产", description: "在目录旁发起扫描，并在扫描队列查看阶段与进度。" },
  }[emptyKind];

  function clearQueryConditions() {
    setQuery("");
    setSearchText("");
    setSelectedTags([]);
    setSkeletonClass("all");
    setActiveMotionFormat("all");
    setActiveSavedFilterId(null);
    setSelected(null);
  }

  function returnToLibrary() {
    clearQueryConditions();
    selectCategory("all");
  }

  return (
    <AppShell className="app-shell" navbar={{ width: { base: 212, sm: 212, md: 224, lg: 236 }, breakpoint: 0 }} aside={{ width: { base: 268, sm: 268, md: 288, lg: 312 }, breakpoint: 0, collapsed: { desktop: !inspectorOpen } }} padding={0} withBorder={false} transitionDuration={0}>
      <AppShell.Navbar className="sidebar">
        <div className="brand-row">
          <div className="brand-mark"><Boxes size={25} strokeWidth={1.5} aria-hidden="true" /></div>
          <div><div className="brand-name">MMDbridge<span>Lib</span></div><div className="brand-subtitle">资产管理</div></div>
        </div>

        <UnstyledButton className={`nav-item library-home ${libraryHomeActive ? "active" : ""}`} aria-current={libraryHomeActive ? "page" : undefined} onClick={() => selectCategory("all")}>
          <Grid2X2 className="nav-icon" size={18} strokeWidth={1.6} aria-hidden="true" /><span>资产库</span><span className="count">{counts.all}</span>
        </UnstyledButton>
        <UnstyledButton className={`nav-item folder-home ${viewMode === "folders" ? "active" : ""}`} aria-current={viewMode === "folders" && !activeRoot ? "page" : undefined} onClick={() => enterFolderView()}>
          <Folder className="nav-icon" size={18} strokeWidth={1.6} aria-hidden="true" /><span>文件夹</span>
        </UnstyledButton>
        <div className="sidebar-section-heading"><span>资产类型</span><ActionIcon variant="subtle" size="sm" aria-label="添加资产根目录" className="icon-button tiny" onClick={addAnyRoot}><Plus size={15} aria-hidden="true" /></ActionIcon></div>
        {(Object.keys(categoryLabels) as AssetType[]).map((type) => (
          <div className={`type-block ${activeType === type ? "type-selected" : ""}`} key={type}>
            <UnstyledButton className="nav-item type-item" aria-current={viewMode === "assets" && activeType === type && !activeRoot && !favoritesOnly && !activeSavedFilterId ? "page" : undefined} onClick={() => selectCategory(type)}>
              <span className={`type-icon ${type}`}><AssetKindIcon type={type} /></span><span>{categoryLabels[type]}</span><span className="count">{counts[type]}</span>
            </UnstyledButton>
            {(activeType === type || activeType === "all") && <div className="root-list">
              {roots.filter((root) => root.assetType === type).map((root) => {
                const rootScan = scanStates.find((scan) => scan.rootId === root.id);
                return <div className={`root-row ${activeRoot === root.id ? "selected" : ""}`} key={root.id}>
                  <UnstyledButton className="root-name" title={root.path} aria-current={activeRoot === root.id ? "page" : undefined} onClick={() => { setActiveType(type); setActiveRoot(root.id); setActiveDirectory(null); setActiveSavedFilterId(null); setFavoritesOnly(false); if (viewMode === "folders") { setQuery(""); setSearchText(""); } setSelected(null); }}>
                    <span className={`root-dot ${root.enabled ? "" : "paused"}`} /><span className="root-label">{root.displayName}</span><span className="count">{counts.byRoot[root.id] ?? 0}</span>
                  </UnstyledButton>
                  {rootScan && activeScanStatuses.has(rootScan.status) &&
                    <span className="root-scan-percent" title="扫描进度">{Math.round(rootScan.progress * 100)}%</span>}
                  <ActionIcon variant="subtle" size="sm" className="root-action root-primary-action" aria-label={rootScan?.status === "Paused" ? `继续 ${root.displayName} 的本次扫描` : `扫描 ${root.displayName}`} title={rootScan?.status === "Paused" ? "继续本次扫描" : "扫描目录"} disabled={busy || !root.enabled} onClick={() => void scanRoot(root)}>{rootScan?.status === "Paused" ? <Play size={14} aria-hidden="true" /> : <RefreshCw size={14} aria-hidden="true" />}</ActionIcon>
                  <ActionIcon variant="subtle" size="sm" className="root-action root-more-action" aria-label={`${root.displayName} 更多操作`} aria-haspopup="menu" aria-expanded={rootMenu?.root.id === root.id} title="更多目录操作" onClick={(event) => openRootMenu(event, root)}><MoreHorizontal size={16} aria-hidden="true" /></ActionIcon>
                </div>;
              })}
            </div>}
          </div>
        ))}

        <div className="sidebar-divider" />
        <UnstyledButton className={`nav-item subdued ${favoritesOnly ? "active" : ""}`} aria-current={favoritesOnly ? "page" : undefined} onClick={() => { setFavoritesOnly(true); setActiveSavedFilterId(null); setActiveType("all"); setViewMode("assets"); setActiveRoot(null); setActiveDirectory(null); setSelected(null); setQuery(""); setSearchText(""); }}><Star className="nav-icon" size={18} strokeWidth={1.6} aria-hidden="true" /><span>收藏</span><span className="count">{favoritesOnly ? assets.length : ""}</span></UnstyledButton>
        <div className="sidebar-section-heading smart-filter-heading"><span>智能集合</span><ActionIcon variant="subtle" size="sm" aria-label="新建智能集合" className="icon-button tiny" onClick={() => setFilterBuilderOpen((open) => !open)}><Plus size={15} aria-hidden="true" /></ActionIcon></div>
        {savedFilters.map((filter) => {
          const retired = hasRetiredFilterCondition(filter.expression);
          return <div className={`smart-filter-row ${retired ? "retired-filter" : ""} ${activeSavedFilterId === filter.id ? "selected" : ""}`} key={filter.id}>
            <UnstyledButton className="nav-item smart-filter-item" aria-current={activeSavedFilterId === filter.id ? "page" : undefined} title={retired ? `${filter.name} · 条件已停用，需要编辑` : filter.name} onClick={() => selectSavedFilter(filter)}><SlidersHorizontal className="nav-icon" size={17} strokeWidth={1.6} aria-hidden="true" /><span>{filter.name}{retired && <small className="smart-filter-warning">条件已停用，需要编辑</small>}</span></UnstyledButton>
            <ActionIcon variant="subtle" size="sm" className="smart-filter-remove" aria-label={`删除智能集合 ${filter.name}`} title="删除智能集合" onClick={() => void removeSmartFilter(filter)}><X size={14} aria-hidden="true" /></ActionIcon>
          </div>;
        })}

        <div className="sidebar-bottom">
          <Button className="sidebar-add" fullWidth leftSection={<Plus size={16} aria-hidden="true" />} onClick={addAnyRoot}>添加资产目录</Button>
          <div className="storage-label"><span>本地资产库</span><span>{roots.length} 个目录</span></div>
          <div className="storage-foot"><span>{counts.all.toLocaleString()} 项资产</span><span className="online"><i />{busy ? "处理中" : isRefreshing ? "读取中" : activeScans.length ? `${activeScans.length} 项扫描中` : activeThumbnailCount ? `${activeThumbnailCount} 项缩略图处理中` : assetQueryError ? "读取失败" : "就绪"}</span></div>
        </div>
      </AppShell.Navbar>

      <AppShell.Main className="main-area">
        <header className="topbar">
          <form className="search-box" onSubmit={submitSearch}>
            <Search size={17} strokeWidth={1.8} aria-hidden="true" /><TextInput value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索名称、文件名、路径或标签（空格分词）…" aria-label="搜索资产" />
            {query && <ActionIcon variant="subtle" size="sm" type="button" className="clear-search" aria-label="清除资产搜索" onClick={() => { setQuery(""); setSearchText(""); }}><X size={14} aria-hidden="true" /></ActionIcon>}
            <kbd>Enter</kbd>
          </form>
          <Button variant={filterBuilderOpen ? "filled" : "light"} aria-expanded={filterBuilderOpen} title="组合筛选并保存为智能集合" onClick={() => setFilterBuilderOpen((open) => !open)} leftSection={<SlidersHorizontal size={16} aria-hidden="true" />} rightSection={<ChevronDown size={13} aria-hidden="true" />}>筛选</Button>
          <Button variant={scanQueueOpen ? "light" : "subtle"} leftSection={<ListTodo size={16} aria-hidden="true" />} onClick={() => setScanQueueOpen(true)}>扫描队列 {activeScans.length ? `(${activeScans.length})` : ""}</Button>
          <ActionIcon variant="subtle" className="journal-button" aria-label="操作日志" title="操作日志" onClick={() => void openOperationJournal()}><History size={18} aria-hidden="true" /></ActionIcon>
          <ActionIcon variant={inspectorOpen ? "light" : "subtle"} className="inspector-toggle" aria-label={inspectorOpen ? "隐藏资产详情" : "显示资产详情"} aria-pressed={inspectorOpen} title="资产详情" onClick={() => setInspectorOpen((opened) => !opened)}><PanelRight size={18} aria-hidden="true" /></ActionIcon>
          <ActionIcon variant="subtle" size="sm" className="avatar" aria-label="设置" title="设置" onClick={() => void openSettings()}><Settings2 size={18} aria-hidden="true" /></ActionIcon>
        </header>

        <div className={`library-content${viewMode === "folders" ? " browsing-folders" : ""}`} ref={setLibraryScrollParent} onScroll={(event) => {
          const scroller = event.currentTarget;
          if (viewMode === "folders") setFolderBrowserCollapse(Math.min(1, Math.max(0, scroller.scrollTop / 240)));
          if (scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 900) void loadNextAssetPage();
        }}>
          <div className="page-heading">
            <div className="heading-copy"><h1>{activeTitle}<span className="heading-count">{(showIndexedTotal ? indexedTotal : visibleAssets.length).toLocaleString()}{!showIndexedTotal && nextAssetCursor ? "+" : ""}</span></h1></div>
            <div className="view-controls"><Button variant="filled" className="open-model-button" leftSection={<Plus size={16} aria-hidden="true" />} onClick={() => void openModelPreview()}>打开模型</Button><label className="card-size-control">视图大小 <Slider  min={130} max={300} step={10} value={cardSize} thumbLabel="资产卡片大小" onChange={(value) => setCardSize(value)} /></label></div>
          </div>

          <Tabs value={activeType} onChange={(value) => { if (value) selectCategory(value as AssetType | "all", viewMode === "folders"); }}>
          <Tabs.List className="library-type-tabs" aria-label="资产类型过滤">
            <Tabs.Tab value="all">全部 <span>{counts.all}</span></Tabs.Tab>
            {(Object.keys(categoryLabels) as AssetType[]).map((type) => <Tabs.Tab value={type} key={type}>{categoryLabels[type]} <span>{counts[type]}</span></Tabs.Tab>)}
            <div className="tabs-spacer" />
            <Button className="quick-add" aria-pressed={bulkSelectMode} disabled={busy} onClick={() => { setBulkSelectMode((mode) => !mode); setBulkSelectedIds(new Set()); }}>{bulkSelectMode ? "退出批量选择" : "批量选择"}</Button>
            {activeType !== "all" && <Button className="quick-add" onClick={() => void addRoot(activeType)}><Plus size={14} aria-hidden="true" /> 添加目录</Button>}
          </Tabs.List>
          </Tabs>
          <div className="quick-filters" aria-label="标签与骨架筛选">
            <MultiSelect className="tag-filter-select" aria-label="标签筛选" placeholder="搜索并选择标签…" data={tagNames} value={selectedTags} onChange={setSelectedTags} searchable clearable hidePickedOptions maxValues={24} limit={80} nothingFoundMessage="没有匹配标签" clearButtonProps={{ "aria-label": "清除标签筛选" }} comboboxProps={{ zIndex: 310 }} size="xs" />
            {selectedTags.length > 1 && <NativeSelect aria-label="标签组合方式" value={tagMatch} onChange={(event) => setTagMatch(event.target.value as "and" | "or")}><option value="and">全部标签</option><option value="or">任一标签</option></NativeSelect>}
            {(activeType === "all" || activeType === "model") && <NativeSelect aria-label="骨架筛选" value={skeletonClass} onChange={(event) => setSkeletonClass(event.target.value as AssetViewState["skeletonClass"])}><option value="all">全部骨架</option><option value="standard">MMD 标准人形</option><option value="nonstandard">非标准</option><option value="unknown">待分类</option></NativeSelect>}
            {quickExpression && <Button onClick={() => { setSelectedTags([]); setSkeletonClass("all"); }}>清除筛选</Button>}
          </div>
          {activeType === "motion" && !activeSavedFilterId && <Tabs value={activeMotionFormat} onChange={(value) => { if (value) selectMotionFormat(value as MotionFormat); }} variant="pills" mb="sm"><Tabs.List aria-label="动作文件格式">
            {([ ["all", "全部动作"], ["vmd", "VMD 动作"], ["vpd", "VPD 姿势"] ] as Array<[MotionFormat, string]>).map(([format, label]) =>
              <Tabs.Tab key={format} value={format}>{label}</Tabs.Tab>)}
          </Tabs.List></Tabs>}
          {viewMode === "folders" && <section className={`folder-browser${folderBrowserCompact ? " compact" : ""}`} aria-label="按文件夹浏览资产">
            <div className="folder-browser-toolbar">
              <div className="folder-breadcrumbs">
                {folderRoot ? <>
                  <Button onClick={() => { setActiveType("all"); setActiveRoot(null); setActiveDirectory(null); setSelected(null); }}>全部根目录</Button>
                  <Button onClick={() => { setActiveDirectory(null); setSelected(null); }}>{folderRoot.displayName}</Button>
                  {(activeDirectory ?? folderRoot.path).slice(folderRoot.path.length).split(/[\\/]/).filter(Boolean).map((part, index, all) => <Button key={`${part}-${index}`} onClick={() => { setActiveDirectory(`${folderRoot.path.replace(/[\\/]+$/, "")}\\${all.slice(0, index + 1).join("\\")}`); setSelected(null); }}>› {part}</Button>)}
                </> : <strong>全部根目录</strong>}
              </div>
              <div className="folder-view-actions"><Button className="folder-return-assets" aria-expanded={!folderBrowserCompact} onClick={() => setFolderBrowserCollapse(folderBrowserCompact ? 0 : 1)}>{folderBrowserCompact ? "展开目录" : "收起目录"}</Button>
                {folderRoot && <span className="folder-visible-count">{indexedTotal.toLocaleString()} 项</span>}
                <Checkbox className="recursive-scope-toggle" checked={recursiveScope} onChange={(event) => setRecursiveScope(event.target.checked)} label={<>包含子目录</>} />
                <Button className="folder-return-assets" onClick={returnToAssetView}>返回资产</Button>
              </div>
            </div>
            {!folderRoot ? <div className="folder-root-grid">{roots.filter((root) => activeType === "all" || root.assetType === activeType).map((root) => <UnstyledButton key={root.id} className="folder-root-card" onClick={() => { setActiveRoot(root.id); setActiveDirectory(null); setActiveType(root.assetType); setActiveMotionFormat("all"); setActiveSavedFilterId(null); setFavoritesOnly(false); setSelected(null); }} title={root.path}><span className={`type-icon ${root.assetType}`}><AssetKindIcon type={root.assetType} /></span><strong>{root.displayName}</strong><small>{(counts.byRoot[root.id] ?? 0).toLocaleString()} 项 · {root.path}</small></UnstyledButton>)}{!roots.length && <UnstyledButton className="folder-root-card add-folder-root" onClick={addAnyRoot}><Plus size={18} aria-hidden="true" /> 添加资产根目录</UnstyledButton>}</div>
              : <div className="folder-tree" aria-label={`${folderRoot.displayName} 的目录层级`}>{renderFolderNodes(folderTree)}</div>}
          </section>}

          {bulkSelectMode && <div className="bulk-actions" aria-label="批量操作">
            <span>已选择 {bulkSelectedIds.size} 项</span>
            <Button disabled={busy || visibleAssets.length === 0} title="仅选择当前已加载的资产，不包含尚未加载的页面" onClick={() => setBulkSelectedIds(new Set(visibleAssets.map((asset) => asset.id)))}>选择已加载（{visibleAssets.length}）</Button>
            <Button disabled={busy || bulkSelectedIds.size === 0} onClick={() => setBulkSelectedIds(new Set())}>清空选择</Button>
            <Button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void setFavoriteForSelection(true)} leftSection={<Star size={14} aria-hidden="true" />}>批量收藏</Button>
            <Button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void setFavoriteForSelection(false)}>取消收藏</Button>
            <Button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void addTagToSelection()} leftSection={<Plus size={14} aria-hidden="true" />}>批量添加标签</Button>
            <Button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void removeTagFromSelection()} leftSection={<Minus size={14} aria-hidden="true" />}>批量移除标签</Button>
            <Button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void planMoveAssets(Array.from(bulkSelectedIds))}>移动资产包…</Button>
            <Button color="red" className="bulk-delete" disabled={busy || !canDeleteBulkPmx} onClick={() => void requestAssetOperation("delete_model", bulkPmxModelIds)}>删除所选模型…</Button>
            <Button color="red" className="bulk-delete" disabled={busy || bulkSelectedIds.size === 0} onClick={() => void requestAssetOperation("recycle", Array.from(bulkSelectedIds))}>移到回收站…</Button>
          </div>}

          {filterBuilderOpen && <section className="filter-builder"><div className="filter-builder-heading"><div><strong>组合筛选</strong><span>将条件保存为可复用的智能集合</span></div><ActionIcon variant="subtle" size="sm" className="icon-button" aria-label="关闭筛选面板" onClick={() => setFilterBuilderOpen(false)}><X size={14} aria-hidden="true" /></ActionIcon></div>
            <div className="filter-builder-name"><label htmlFor="smart-filter-name">集合名称</label><TextInput id="smart-filter-name" value={filterName} onChange={(event) => setFilterName(event.target.value)} placeholder="例如：收藏的模型" /></div>
            <div className="filter-rule-list">{filterRules.map((rule, index) => <div className="filter-rule-row" key={index}>
              <NativeSelect aria-label="筛选字段" value={rule.field} onChange={(event) => changeFilterField(index, event.target.value as FilterField)}>{(Object.keys(filterFieldLabels) as FilterField[]).filter((field) => field !== "duplicateStatus" && field !== "cameraOnly" && field !== "needsReview").map((field) => <option key={field} value={field}>{filterFieldLabels[field]}</option>)}</NativeSelect>
              <NativeSelect aria-label="比较方式" value={rule.operator} onChange={(event) => changeFilterRule(index, { operator: event.target.value as FilterOperator })}>{operatorsFor(rule.field).map((operator) => <option key={operator} value={operator}>{operatorLabels[operator]}</option>)}</NativeSelect>
              {booleanFilterFields.has(rule.field) ? <NativeSelect aria-label="布尔值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="true">是</option><option value="false">否</option></NativeSelect>
                : rule.field === "assetType" ? <NativeSelect aria-label="资产类型值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="model">模型</option><option value="motion">动作</option><option value="scene">场景</option></NativeSelect>
                : rule.field === "rootId" && roots.length ? <NativeSelect aria-label="资产根目录值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="">选择目录</option>{roots.map((root) => <option key={root.id} value={root.id}>{root.displayName}</option>)}</NativeSelect>
                : rule.field === "skeletonClass" ? <NativeSelect aria-label="骨架分类值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="standard">MMD 标准人形</option><option value="nonstandard">非标准</option><option value="unknown">待分类</option></NativeSelect>
                : rule.field === "cardStatus" ? <NativeSelect aria-label="资源卡状态值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="CardValid">有效</option><option value="CardMissing">缺失</option><option value="CardStale">过期</option><option value="CardBroken">损坏</option></NativeSelect>
                : dateFilterFields.has(rule.field) ? <TextInput aria-label="日期值" type="datetime-local" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })} />
                : numericFilterFields.has(rule.field) ? <TextInput aria-label="数值" type="number" step="any" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })} />
                : <TextInput aria-label="文本值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })} placeholder="输入匹配内容" />}
              <Checkbox className="filter-not" checked={rule.negate} onChange={(event) => changeFilterRule(index, { negate: event.target.checked })} label={<>取反</>} />
              <ActionIcon variant="subtle" size="sm" className="filter-rule-remove" aria-label="移除此条件" disabled={filterRules.length <= 1} onClick={() => setFilterRules((current) => current.filter((_, ruleIndex) => ruleIndex !== index))}><X size={14} aria-hidden="true" /></ActionIcon>
            </div>)}</div>
            <div className="filter-builder-footer"><Button className="filter-add-rule" onClick={() => setFilterRules((current) => [...current, { field: "tag", operator: "contains", value: "", negate: false }])} leftSection={<Plus size={14} aria-hidden="true" />}>添加条件</Button><label className="filter-group-op">条件组合<NativeSelect value={filterGroupOp} onChange={(event) => setFilterGroupOp(event.target.value as "and" | "or")}><option value="and">全部满足（AND）</option><option value="or">任一满足（OR）</option></NativeSelect></label><span className="filter-builder-spacer" /><Button className="filter-save" disabled={!filterName.trim()} onClick={() => void saveSmartFilter()}>保存并应用</Button></div>
          </section>}

          {error && <Alert color="red" title="操作未完成" withCloseButton onClose={() => setError("")} closeButtonLabel="关闭错误提示" mb="sm">{error}{error.startsWith("资产读取超时") && <Button ml="sm" onClick={() => { setError(""); void refresh(); }}>重新读取</Button>}</Alert>}
          {notice && <Alert color="mint" title={busy ? "正在处理" : undefined} withCloseButton onClose={() => setNotice("")} closeButtonLabel="关闭任务提示" mb="sm" role="status">{notice}</Alert>}

          {viewMode === "folders" && !folderRoot ? null : isRefreshing ? <div className="asset-grid-waiting" role="status">正在读取资产…</div> : visibleAssets.length ? libraryScrollParent ? <VirtuosoGrid
            data={visibleAssets}
            customScrollParent={libraryScrollParent}
            increaseViewportBy={{ top: 360, bottom: 720 }}
            endReached={() => void loadNextAssetPage()}
            computeItemKey={(_, asset) => asset.id}
            listClassName="asset-grid"
            style={{ "--card-min-width": `${cardSize}px` } as React.CSSProperties}
            itemClassName="asset-grid-item"
            itemContent={(_, asset) => {
              const status = importantStatus(asset);
              return <UnstyledButton className={`asset-card ${bulkSelectMode ? "bulk-selectable" : ""} ${selected?.id === asset.id ? "selected" : ""} ${bulkSelectedIds.has(asset.id) ? "bulk-selected" : ""}`} aria-label={`${asset.name} · ${categoryLabels[asset.assetType]}${status ? ` · ${status.text}` : ""}${asset.isFavorite ? " · 已收藏" : ""}`} aria-pressed={bulkSelectMode ? bulkSelectedIds.has(asset.id) : selected?.id === asset.id} onClick={() => toggleBulkSelection(asset)} onDoubleClick={() => { if (!bulkSelectMode) openAsset3D(asset); }} onContextMenu={(event) => openAssetMenu(event, asset)} onKeyDown={(event) => {
                if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) openAssetMenu(event, asset);
                else if (event.key === "Enter" && !bulkSelectMode) { event.preventDefault(); openAsset3D(asset); }
              }}>
              <div className={`asset-art ${asset.assetType} ${asset.hasThumbnail ? "has-thumbnail" : ""}`}>
                {asset.hasThumbnail && <CardThumbnail revision={thumbnailRevisions[asset.id]} assetId={asset.id} alt={`${asset.name} 缩略图`} />}
                {!asset.hasThumbnail && <div className="asset-placeholder"><AssetKindIcon type={asset.assetType} size={36} /><span>尚未生成预览</span></div>}<span className="art-format">{String(asset.metadata.file_type ?? asset.assetType).toUpperCase()}</span>
                <div className="asset-markers">{asset.isFavorite && <span className="favorite-marker" role="img" aria-label="已收藏" title="已收藏"><Star size={14} fill="currentColor" aria-hidden="true" /></span>}{typeof asset.metadata.paired_camera_path === "string" && <span className="paired-camera-marker" title={`配套镜头：${asset.metadata.paired_camera_path}`}><Camera size={12} aria-hidden="true" /> 镜头</span>}{asset.assetType === "model" && asset.metadata.skeleton_class === "nonstandard" && <span className="skeleton-badge">非标准</span>}{status && <span className="asset-status-marker" data-tone={status.tone} title={status.text}>{status.text}</span>}</div>
              </div>
              <div className="asset-card-body"><div className="asset-card-title" title={asset.name}>{asset.name}</div><div className="asset-card-subline"><Badge size="xs" variant="light" color={asset.assetType === "motion" ? "violet" : asset.assetType === "scene" ? "yellow" : "mint"}>{asset.metadata.is_pose ? "姿势" : categoryLabels[asset.assetType]}</Badge><span className="asset-source-name" title={asset.primarySource}>{assetSummary(asset)}</span></div></div>
              {bulkSelectMode && <span className={`asset-bulk-checkbox ${bulkSelectedIds.has(asset.id) ? "checked" : ""}`} aria-hidden="true">{bulkSelectedIds.has(asset.id) ? <Check size={14} aria-hidden="true" /> : null}</span>}
            </UnstyledButton>;
            }}
          /> : <div className="asset-grid-waiting" role="status">正在载入资产卡片…</div> : <section className="empty-state">
            <div className="empty-illustration"><FolderOpen size={48} strokeWidth={1.2} aria-hidden="true" /></div>
            <h2>{emptyCopy.title}</h2>
            <p>{emptyCopy.description}</p>
            <div className="empty-actions">{emptyKind === "no-roots" ? (Object.keys(categoryLabels) as AssetType[]).map((type) => <Button key={type} onClick={() => void addRoot(type)}><span><AssetKindIcon type={type} /></span>添加{categoryLabels[type]}目录</Button>)
              : emptyKind === "error" ? <Button disabled={isRefreshing} onClick={() => void refresh()}>重新读取</Button>
                : emptyKind === "filter" ? <Button onClick={clearQueryConditions}>清除查询条件</Button>
                  : emptyKind === "favorite" ? <Button onClick={returnToLibrary}>返回资产库</Button>
                    : <>{emptyKind === "folder" && <Button onClick={returnToLibrary}>返回资产库</Button>}<Button onClick={() => setScanQueueOpen(true)}>查看扫描队列</Button></>}</div>
            <div className="privacy-note">移除目录仅移除索引，不会删除源文件。</div>
          </section>}
          {!isRefreshing && (nextAssetCursor || loadingNextPage) && <div className="asset-grid-footer" ref={loadMoreRef} role="status">{loadingNextPage ? "正在载入更多资产…" : <><span>已载入 {visibleAssets.length.toLocaleString()} 项</span><Button onClick={() => void loadNextAssetPage()}>加载更多</Button></>}</div>}
        </div>

      <footer className={`jobbar ${jobsExpanded ? "expanded" : ""}`}>
        <div className="jobbar-leading"><span className={`jobbar-indicator ${busy || activeThumbnailCount || activeScans.length ? "working" : ""}`} />{busy ? "正在处理资产" : "后台任务"}<span className="jobbar-count">{activeThumbnailCount + activeScans.length + (busy ? 1 : 0)}</span></div>
        <div className="jobbar-detail">
          {busy ? notice : activeScans.length ? `${activeScans.length} 个扫描任务 · ${scanStatusLabels[activeScans[0].status]} ${Math.round(activeScans[0].progress * 100)}%` : activeThumbnailCount ? `${activeThumbnailCount} 个缩略图任务 · 已完成 ${jobSummary.Completed ?? 0} · 失败 ${jobSummary.Failed ?? 0}` : "没有进行中的任务"}
        </div>
        {scanStates.length > 0 && <Button className="jobbar-scan-link" onClick={() => setScanQueueOpen(true)}>管理扫描</Button>}
        {totalThumbnailCount > 0 && <Button className="jobbar-chevron" aria-label={jobsExpanded ? "收起缩略图任务" : "展开缩略图任务"} onClick={() => setJobsExpanded((expanded) => !expanded)}>{jobsExpanded ? <ChevronDown size={15} aria-hidden="true" /> : <ChevronUp size={15} aria-hidden="true" />}</Button>}
        {jobsExpanded && <div className="jobs-panel" aria-label="后台任务列表">
          <div className="jobs-panel-heading"><strong>缩略图任务</strong><span>{totalThumbnailCount} 项 · 显示最近 12 项</span></div>
          {jobs.length ? jobs.slice(0, 12).map((job) => <div className="jobs-panel-row" key={job.id}>
            <div className="jobs-panel-main"><strong title={job.asset_id ?? undefined}>{job.asset_id ? assetsById.get(job.asset_id)?.name ?? job.asset_id : "缩略图"}</strong><span>{jobStatusLabels[job.status] ?? job.status}{activeThumbnailStatuses.has(job.status) ? ` · ${Math.round(job.progress * 100)}%` : job.error?.message ? ` · ${job.error.message}` : ""}</span></div>
            {activeJobs.some((activeJob) => activeJob.id === job.id) && <Button disabled={job.status === "Cancelling"} onClick={() => void cancelJob(job.id)}>取消</Button>}
            {["Failed", "Cancelled"].includes(job.status) && <Button onClick={() => void retryJob(job.id)}>重试</Button>}
          </div>) : <div className="jobs-panel-empty">没有缩略图任务</div>}
        </div>}
      </footer>
      </AppShell.Main>

      <AppShell.Aside className={`inspector ${selected ? "has-selection" : ""}`} aria-hidden={!inspectorOpen}>
        <div className="inspector-top"><div><div className="eyebrow">当前选择</div><h2>资产详情</h2></div><ActionIcon variant="subtle" size="sm" className="icon-button" aria-label="关闭资产详情" onClick={() => setInspectorOpen(false)}><X size={17} aria-hidden="true" /></ActionIcon></div>
        {selected ? <>
          <UnstyledButton className={`inspector-preview ${selected.assetType} ${selected.hasThumbnail ? "has-thumbnail" : ""}`} title="查看缩略图" onClick={() => setPreviewAsset(selected)}>{selected.hasThumbnail && <CardThumbnail revision={thumbnailRevisions[selected.id]} assetId={selected.id} alt={`${selected.name} 缩略图`} />}{!selected.hasThumbnail && <div className="asset-placeholder"><AssetKindIcon type={selected.assetType} size={38} /><span>尚未生成预览</span></div>}<span className="preview-badge">{selected.metadata.is_pose ? "姿势" : categoryLabels[selected.assetType]}</span></UnstyledButton>
          <div className="inspector-title"><div><div className="inspector-type">{categoryLabels[selected.assetType]}</div><h3>{selected.name}</h3></div><Button color={selected.isFavorite ? "yellow" : "gray"} aria-label={selected.isFavorite ? "取消收藏" : "添加收藏"} aria-pressed={selected.isFavorite} title={selected.isFavorite ? "取消收藏" : "添加收藏"} onClick={() => void toggleFavorite(selected)}><Star size={18} fill={selected.isFavorite ? "currentColor" : "none"} aria-hidden="true" /></Button></div>
          <div className="inspector-section"><div className="section-title">基本信息</div><div className="detail-list">
            {Object.entries(selected.metadata).filter(([key, value]) => metadataLabels[key] && (typeof value === "number" || typeof value === "boolean" || typeof value === "string")).slice(0, 10).map(([key, value]) => <div className="detail-row" key={key}><span>{metadataLabels[key]}</span><strong>{formatValue(value)}</strong></div>)}
            <div className="detail-row"><span>资产状态</span><strong className={selected.statuses.includes("ParseFailed") || selected.statuses.includes("MissingSource") ? "state-warn" : "state-ready"}>{assetStatusText(selected.statuses)}</strong></div>
            <div className="detail-row"><span>资源卡</span><strong className={selected.cardStatus === "CardValid" ? "state-ready" : "state-muted"}>{cardStatusText(selected.cardStatus)}{!selected.hasThumbnail ? " · 尚无缩略图" : ""}</strong><span className="detail-row-actions">{(selected.cardStatus !== "CardValid" || !selected.hasThumbnail) && <Button className="tiny-link" disabled={busy} onClick={() => void createCard(selected.id)}>创建 / 刷新</Button>}<Button className="tiny-link" disabled={busy} onClick={() => void verifyCard(selected.id)}>校验</Button></span></div>
          </div></div>
          {selected.assetType === "model" && <div className="detail-row"><span>骨架分类</span><strong>{selected.metadata.skeleton_class === "standard" ? "MMD 标准人形" : selected.metadata.skeleton_class === "nonstandard" ? "非标准" : "待分类"}</strong></div>}
          <div className="inspector-section source-section"><div className="section-title">源文件</div><div className="source-path" title={selected.primarySource}><FileText className="file-icon" size={17} aria-hidden="true" /><div><strong>{selected.primarySource.split(/[\\/]/).pop()}</strong><small>{selected.assetDirectory}</small></div></div><div className="asset-file-actions"><Button disabled={busy} onClick={() => void planRenameAsset(selected)}>重命名资产包</Button><Button disabled={busy} onClick={() => void planMoveAssets([selected.id])}>移动…</Button>{selected.assetType === "model" && selected.primarySource.toLowerCase().endsWith(".pmx") ? <Button color="red" className="danger" disabled={busy} onClick={() => void requestAssetOperation("delete_model", [selected.id])}>删除模型…</Button> : <Button disabled={busy} onClick={() => void requestAssetOperation("recycle", [selected.id])}>移到回收站…</Button>}</div></div>
          <div className="inspector-section tags-section"><div className="section-title">标签 <ActionIcon variant="subtle" size="sm" className="add-tag" aria-label="添加用户标签" title="添加用户标签" disabled={!detailsReady || busy} onClick={() => void addTag()}><Plus size={15} aria-hidden="true" /></ActionIcon></div>{currentDetails?.status === "failed" ? <Alert color="red" title="标签与关系读取失败" role="alert">{currentDetails.error}<Button disabled={busy} onClick={() => setSelectedDetailsRevision((value) => value + 1)}>重试详情</Button></Alert> : !detailsReady ? <div className="tag-empty" role="status">正在加载标签与关系…</div> : assetTags.length ? <div className="tag-list">{assetTags.map((tag) => <span className={`tag-chip ${tag.source}`} key={`${tag.name}-${tag.source}`} title={`来源：${tag.source}`}><span className="tag-chip-name">{tag.name}</span><ActionIcon variant="subtle" size="xs" aria-label={`移除标签 ${tag.name}`} disabled={!detailsReady || busy} onClick={() => void removeTag(tag.name)}><X size={14} aria-hidden="true" /></ActionIcon></span>)}</div> : <div className="tag-empty">尚未添加标签</div>}</div>
          {assetRelations.length > 0 && <div className="inspector-section relation-section"><div className="section-title">关系与版本 <span className="relation-count">{assetRelations.length}</span></div><div className="relation-list">{assetRelations.map((relation) => {
            const otherPath = selected.id === relation.sourceAsset ? relation.reason.target_path : relation.reason.source_path;
            const otherName = typeof otherPath === "string" ? otherPath.split(/[\\/]/).pop() : (selected.id === relation.sourceAsset ? relation.targetAsset : relation.sourceAsset).slice(0, 8);
            const reasonCodes = Array.isArray(relation.reason.reason_codes) ? relation.reason.reason_codes.filter((reason): reason is string => typeof reason === "string") : [];
            const relationName = relation.relationType === "MotionCameraPair" ? "动作 / Camera" : "版本族";
            const canPreviewCamera = relation.relationType === "MotionCameraPair" && relation.sourceAsset === selected.id && selected.assetType === "motion" && selected.primarySource.toLowerCase().endsWith(".vmd");
            return <div className="relation-card" key={relation.id} title={reasonCodes.join(" · ")}><div className="relation-card-main"><strong>{relationName}</strong><span>{otherName}</span></div>{typeof otherPath === "string" && <small className="relation-card-path" title={otherPath}>{otherPath}</small>}<div className="relation-card-meta"><span>{Math.round(relation.confidence * 100)}% · {relation.confirmed ? "已确认" : "待确认"}</span>{canPreviewCamera ? <Button className="tiny-link" disabled={!detailsReady || busy} onClick={() => void selectCameraAndPreview(relation, selected)}>{relation.confirmed ? "预览此镜头" : "选择并预览"}</Button> : !relation.confirmed && <Button className="tiny-link" disabled={!detailsReady || busy} onClick={() => void confirmRelation(relation)}>确认</Button>}</div></div>;
          })}</div></div>}
          <div className="inspector-spacer" />
          <div className="inspector-actions"><Button onClick={() => void revealAsset(selected)} title="在资源管理器中定位源文件"><ArrowUpRight size={15} aria-hidden="true" /> 在资源管理器中定位</Button><Button onClick={() => viewAssetDirectory(selected)} title="在 MMDbridgeLib 文件夹视图中打开">查看所在文件夹</Button><Button onClick={() => void openAssetDirectory(selected)} title="在资源管理器中打开资产源目录">在资源管理器中打开</Button>{selected.assetType !== "motion" && <Button className="preview-3d-button" onClick={() => setViewerAsset({ id: selected.id, name: selected.name, primarySource: selected.primarySource, assetType: selected.assetType as "model" | "scene" })}><Box size={16} aria-hidden="true" /> 3D 预览</Button>}{selected.assetType === "motion" && selected.primarySource.toLowerCase().endsWith(".vmd") && <Button className="preview-3d-button" onClick={() => setMotionViewerAsset(selected)}><Play size={16} aria-hidden="true" /> 3D 动作</Button>}</div>
        </> : <div className="inspector-empty"><div className="inspector-empty-icon"><Box size={28} strokeWidth={1.4} aria-hidden="true" /></div><strong>选择一个资产</strong><span>详细信息将在此处显示</span></div>}
      </AppShell.Aside>
      {dialogs.dialog}
      {assetMenu && <LibraryContextMenu x={assetMenu.x} y={assetMenu.y} label={`${assetMenu.asset.name} 操作`} onClose={() => setAssetMenu(null)}><Menu.Item role="menuitem" onClick={() => { setPreviewAsset(assetMenu.asset); setAssetMenu(null); }}>查看缩略图</Menu.Item>{assetMenu.asset.assetType !== "motion" && <Menu.Item role="menuitem" onClick={() => { setViewerAsset({ id: assetMenu.asset.id, name: assetMenu.asset.name, primarySource: assetMenu.asset.primarySource, assetType: assetMenu.asset.assetType as "model" | "scene" }); setAssetMenu(null); }}>查看 3D {assetMenu.asset.assetType === "scene" ? "场景" : "模型"}</Menu.Item>}{assetMenu.asset.assetType === "motion" && assetMenu.asset.primarySource.toLowerCase().endsWith(".vmd") && <Menu.Item role="menuitem" onClick={() => { setMotionViewerAsset(assetMenu.asset); setAssetMenu(null); }}>播放 3D 动作{typeof assetMenu.asset.metadata.paired_camera_path === "string" ? " / 配套镜头" : ""}</Menu.Item>}<Menu.Divider /><Menu.Item role="menuitem" onClick={() => { viewAssetDirectory(assetMenu.asset); setAssetMenu(null); }}>查看所在文件夹</Menu.Item><Menu.Item role="menuitem" onClick={() => { void revealAsset(assetMenu.asset); setAssetMenu(null); }}>在资源管理器中定位源文件</Menu.Item><Menu.Item role="menuitem" onClick={() => { void openAssetDirectory(assetMenu.asset); setAssetMenu(null); }}>在资源管理器中打开所在目录</Menu.Item>{assetMenu.asset.assetType === "model" && assetMenu.asset.primarySource.toLowerCase().endsWith(".pmx") && <Menu.Item role="menuitem" color="red" disabled={busy} onClick={() => { void requestAssetOperation("delete_model", [assetMenu.asset.id]); setAssetMenu(null); }}>删除模型…</Menu.Item>}<Menu.Divider /><Menu.Item role="menuitem" onClick={() => { void toggleFavorite(assetMenu.asset); setAssetMenu(null); }}>{assetMenu.asset.isFavorite ? "取消收藏" : "添加收藏"}</Menu.Item><Menu.Item role="menuitem" disabled={busy} onClick={() => { void createCard(assetMenu.asset.id); setAssetMenu(null); }}>重新生成此缩略图</Menu.Item></LibraryContextMenu>}
      {rootMenu && <LibraryContextMenu x={rootMenu.x} y={rootMenu.y} label={`${rootMenu.root.displayName} 目录操作`} onClose={() => setRootMenu(null)} width={280}><Menu.Item role="menuitem" disabled={!!activeScans.length} onClick={() => { renameRoot(rootMenu.root); setRootMenu(null); }}>修改显示名称</Menu.Item><Menu.Item role="menuitem" disabled={!!activeScans.length} onClick={() => { void updateRoot(rootMenu.root, { enabled: !rootMenu.root.enabled }); setRootMenu(null); }}>{rootMenu.root.enabled ? "停用目录监视与后续扫描" : "启用目录监视与后续扫描"}</Menu.Item><Menu.Item role="menuitem" disabled={!!activeScans.length} title="设置影响后续扫描；关闭递归时，现有子目录索引状态会保留。" onClick={() => { void updateRoot(rootMenu.root, { scanRecursive: !rootMenu.root.scanRecursive }); setRootMenu(null); }}>递归扫描：{rootMenu.root.scanRecursive ? "已开启 · 点击关闭" : "已关闭 · 点击开启"}</Menu.Item><Menu.Divider /><Menu.Item role="menuitem" disabled={busy || !rootMenu.root.enabled || scanStates.some((scan) => scan.rootId === rootMenu.root.id && (activeScanStatuses.has(scan.status) || scan.status === "Paused"))} onClick={() => { void scanRoot(rootMenu.root, true); setRootMenu(null); }}>完整检查源文件与资源卡</Menu.Item><Menu.Item role="menuitem" disabled={busy || !rootMenu.root.enabled || activeScans.some((scan) => scan.rootId === rootMenu.root.id)} onClick={() => { void queueCards(rootMenu.root); setRootMenu(null); }}>生成资源卡和缩略图</Menu.Item><Menu.Divider /><Menu.Item role="menuitem" className="root-menu-remove" disabled={!!activeScans.length} onClick={() => { void removeRoot(rootMenu.root); setRootMenu(null); }}>从资产库移除目录</Menu.Item></LibraryContextMenu>}
      {scanQueueOpen && <Modal opened onClose={() => { setScanQueueOpen(false); }} title={<div><strong id="scan-queue-title">扫描索引队列</strong></div>} size={900} zIndex={200} closeOnClickOutside={true} closeOnEscape={true} closeButtonProps={{ "aria-label": "关闭窗口" }} classNames={{ content: "library-modal", title: "library-modal-title", body: "operation-modal scan-queue-modal" }}><p className="operation-summary">扫描按队列顺序依次执行。暂停会保留已建立的索引；继续时会重新核对该目录。</p><div className="scan-queue-list">{scanStates.length ? scanStates.map((scan) => {
          const root = roots.find((item) => item.id === scan.rootId);
          const pending = scanStates.filter((item) => item.status === "Pending").sort((a, b) => a.queueOrder - b.queueOrder);
          const pendingIndex = pending.findIndex((item) => item.rootId === scan.rootId);
          return <div className="scan-queue-row" key={scan.rootId}>
            <div className="scan-queue-main"><strong>{root?.displayName ?? scan.rootId}{scan.scope === "local" ? " · 局部更新" : scan.scope === "full" ? " · 完整发现" : ""}{scan.fullCheck ? " · 深度检查" : ""}</strong><span>{scanStatusLabels[scan.status] ?? scan.status} · {scan.filesProcessed.toLocaleString()}/{scan.filesSeen.toLocaleString()} 文件 · {Math.round(scan.progress * 100)}%{scan.error ? ` · ${scan.error}` : ""}</span><Progress value={scan.progress * 100} aria-label={`${root?.displayName ?? "目录"}扫描进度`} size="xs" mt={6} /></div>
            <div className="scan-queue-actions">
              {scan.status === "Pending" && <><Button disabled={pendingIndex <= 0} aria-label={`${root?.displayName ?? "扫描"}上移`} onClick={() => void moveScan(scan.rootId, -1)}>↑</Button><Button disabled={pendingIndex >= pending.length - 1} aria-label={`${root?.displayName ?? "扫描"}下移`} onClick={() => void moveScan(scan.rootId, 1)}>↓</Button></>}
              {["Pending", "Discovering", "Indexing", "Verifying", "Relations"].includes(scan.status) && <Button onClick={() => void pauseScan(scan.rootId)}>暂停</Button>}
              {scan.status === "Paused" && root?.enabled && <Button onClick={() => void scanRoot(root)}>继续</Button>}
              {activeScanStatuses.has(scan.status) || scan.status === "Paused" ? <Button onClick={() => void cancelScan(scan.rootId)}>停止</Button> : null}
              {["Failed", "Cancelled", "Completed"].includes(scan.status) && root?.enabled && <Button onClick={() => void scanRoot(root)}>{scan.status === "Completed" ? "重扫" : "重试"}</Button>}
              {["Failed", "Cancelled", "Completed"].includes(scan.status) && root?.enabled && <Button onClick={() => void scanRoot(root, true)}>完整检查</Button>}
            </div>
          </div>;
        }) : <div className="jobs-panel-empty">队列为空。点击目录旁的扫描按钮加入任务。</div>}</div></Modal>}
      {previewAsset && <Modal opened onClose={() => { setPreviewAsset(null); }} title={<div><strong id="thumbnail-preview-title">{previewAsset.name}</strong></div>} size={760} zIndex={200} closeOnClickOutside={true} closeOnEscape={true} closeButtonProps={{ "aria-label": "关闭窗口" }} classNames={{ content: "library-modal", title: "library-modal-title", body: "operation-modal thumbnail-preview-modal" }}><div className="thumbnail-preview-frame">{previewAsset.hasThumbnail ? <CardThumbnail revision={thumbnailRevisions[previewAsset.id]} assetId={previewAsset.id} alt={`${previewAsset.name} 缩略图`} /> : <div className="thumbnail-preview-empty">该资产尚未生成缩略图</div>}</div>{!previewAsset.hasThumbnail && <div className="settings-modal-actions"><Button onClick={() => { void createCard(previewAsset.id); setPreviewAsset(null); }}>生成资源卡和缩略图</Button></div>}</Modal>}
      {viewerAsset && <Suspense fallback={<div role="status" style={{ position: "fixed", inset: 0, zIndex: 50, display: "grid", placeItems: "center", background: "#050a0de8", color: "#bbcbc9", fontSize: 12 }}>正在载入 3D Viewer…</div>}><ModelViewer asset={viewerAsset} onClose={() => setViewerAsset(null)} /></Suspense>}
      {motionViewerAsset && <Suspense fallback={<div role="status" style={{ position: "fixed", inset: 0, zIndex: 50, display: "grid", placeItems: "center", background: "#050a0de8", color: "#bbcbc9", fontSize: 12 }}>正在载入 VMD 3D Viewer…</div>}><MotionViewer asset={motionViewerAsset} onClose={() => setMotionViewerAsset(null)} /></Suspense>}
      {assetOperationPlan && <Modal opened onClose={() => { if (!busy) setAssetOperationPlan(null); }} title={<div><strong id="operation-plan-title">确认资产操作</strong></div>} size={720} zIndex={200} closeOnClickOutside={!busy} closeOnEscape={!busy} closeButtonProps={{ "aria-label": "关闭窗口", disabled: busy }} classNames={{ content: "library-modal", title: "library-modal-title", body: "operation-modal" }}>{assetOperationError && <Alert color="red" role="alert" mb="sm">{assetOperationError}</Alert>}<p className="operation-summary">{assetOperationPlan.operation === "move" ? "移动" : assetOperationPlan.operation === "rename" ? "重命名" : assetOperationPlan.operation === "delete_model" ? (assetOperationPlan.deleteMode === "folder" ? "回收整个模型文件夹" : "只回收所选 PMX 文件") : "发送到 Windows 回收站"}将处理 {assetOperationPlan.sourcePaths.length} 个路径、{assetOperationPlan.affectedAssets.length} 项索引资产。</p>{assetOperationPlan.operation === "delete_model" && <>
          {assetOperationPlan.deleteReason && <div className="operation-warnings"><p>{assetOperationPlan.deleteReason}</p></div>}
          {(assetOperationPlan.pmxDirectories ?? []).map((directory) => <div className="operation-assets" key={directory.path}><strong>同目录 PMX · {directory.pmxPaths.length}</strong><details><summary>查看目录与 PMX 清单</summary><div><span>{directory.path}</span>{directory.pmxPaths.map((path) => <span title={path} key={path}>{path}</span>)}</div></details></div>)}
          {assetOperationPlan.deleteMode === "folder" && assetOperationPlan.packageSnapshots.length > 0 && <div className="operation-assets"><strong>整目录回收内容 · {assetOperationPlan.packageSnapshots.length}</strong><details><summary>展开全部文件与目录</summary><div>{assetOperationPlan.packageSnapshots.map((entry) => <span title={entry.path} key={entry.path}>{entry.path}</span>)}</div></details></div>}
          {assetOperationPlan.deleteMode === "pmxOnly" && (assetOperationPlan.preservedPaths?.length ?? 0) > 0 && <div className="operation-assets"><strong>保留路径 · {assetOperationPlan.preservedPaths?.length}</strong><details><summary>展开保留的文件与目录</summary><div>{assetOperationPlan.preservedPaths?.map((path) => <span title={path} key={path}>{path}</span>)}</div></details><p>同目录的其他模型、纹理、说明文件、文件夹和资源卡保留。</p></div>}
        </>}<div className="operation-path-list">{assetOperationPlan.sourcePaths.map((source, index) => <div className="operation-path-row" key={source}><span>{source}</span>{assetOperationPlan.destinationPaths[index] && <><b>→</b><span>{assetOperationPlan.destinationPaths[index]}</span></>}</div>)}</div>{assetOperationPlan.affectedAssets.length > 0 && <div className="operation-assets"><strong>受影响资产 · {assetOperationPlan.affectedAssets.length}</strong><details><summary>展开全部资产</summary><div>{assetOperationPlan.affectedAssets.map((asset) => <span title={asset.primarySource} key={asset.id}>{asset.name}</span>)}</div></details></div>}{assetOperationPlan.dependencyPaths.length > 0 && <div className="operation-assets"><strong>包内依赖文件 · {assetOperationPlan.dependencyPaths.length}</strong><details><summary>展开全部依赖</summary><div>{assetOperationPlan.dependencyPaths.map((path) => <span title={path} key={path}>{path}</span>)}</div></details></div>}{assetOperationPlan.warnings.length > 0 && <div className="operation-warnings"><strong>安全检查未通过</strong>{assetOperationPlan.warnings.map((warning) => <p key={warning}>{warning}</p>)}</div>}<footer className="operation-modal-actions"><Button disabled={busy} onClick={() => { if (!busy) setAssetOperationPlan(null); }}>取消</Button><Button variant="filled" color={assetOperationPlan.operation === "recycle" || assetOperationPlan.operation === "delete_model" ? "red" : "mint"} disabled={!assetOperationPlan.canExecute || busy} onClick={() => void executePlannedAssetOperation()}>{busy ? "正在执行…" : assetOperationPlan.operation === "delete_model" ? assetOperationPlan.deleteMode === "folder" ? "回收整个文件夹" : "只回收所选 PMX" : assetOperationPlan.operation === "recycle" ? "确认移到回收站" : "确认执行"}</Button></footer></Modal>}
      {operationJournalOpen && <Modal opened onClose={() => { setOperationJournalOpen(false); }} title={<div><strong id="operation-journal-title">资产操作日志</strong></div>} size={900} zIndex={200} closeOnClickOutside={true} closeOnEscape={true} closeButtonProps={{ "aria-label": "关闭窗口" }} classNames={{ content: "library-modal", title: "library-modal-title", body: "operation-modal journal-modal" }}>{operationJournalError && <Alert color="red" role="alert" mb="sm">{operationJournalError}</Alert>}<div className="journal-heading-row"><span>记录保留在本地数据库；“需要检查”表示文件操作部分完成或索引更新失败。</span><Button disabled={busy} onClick={() => void openOperationJournal()}>刷新</Button></div><div className="journal-entry-list">{operationJournal.length ? operationJournal.map((entry) => <article className="journal-entry" key={entry.id}>
          <div className="journal-entry-heading"><strong>{entry.operation === "move" ? "移动资产包" : entry.operation === "rename" ? "重命名资产包" : entry.operation === "delete_model" ? "删除模型" : "移到回收站"}</strong><span className={`journal-status ${entry.status === "RecoveryNeeded" || entry.status === "Started" ? "needs-recovery" : entry.status.toLowerCase()}`}>{entry.status === "Started" ? "进行中" : entry.status === "RecoveryNeeded" ? "需要检查" : entry.status === "Completed" ? "完成" : entry.status === "Resolved" ? "已核对" : "失败"}</span></div>
          <div className="journal-entry-paths">{entry.sourcePaths.map((source, index) => <div key={`${entry.id}-${source}`}><span>{source}</span>{entry.destinationPaths[index] && <><b>→</b><span>{entry.destinationPaths[index]}</span></>}</div>)}</div>
          <div className="journal-entry-result">{entry.affectedAssetCount} 项资产 · {entry.result?.message ?? "没有结果说明"} · {new Date(entry.updatedAt).toLocaleString()}</div>
          {((entry.result?.completedPaths?.length ?? 0) > 0 || (entry.result?.uncertainPaths?.length ?? 0) > 0 || (entry.result?.notStartedPaths?.length ?? 0) > 0) && <div className="journal-action-details">
            {entry.result?.completedPaths?.length ? <details><summary>已完成 · {entry.result.completedPaths.length}</summary>{entry.result.completedPaths.map((path) => <span key={`done-${path}`}>{path}</span>)}</details> : null}
            {entry.result?.uncertainPaths?.length ? <details><summary>需要检查 · {entry.result.uncertainPaths.length}</summary>{entry.result.uncertainPaths.map((path) => <span key={`uncertain-${path}`}>{path}</span>)}</details> : null}
            {entry.result?.notStartedPaths?.length ? <details><summary>未开始 · {entry.result.notStartedPaths.length}</summary>{entry.result.notStartedPaths.map((path) => <span key={`pending-${path}`}>{path}</span>)}</details> : null}
          </div>}
          {entry.status === "RecoveryNeeded" && <Button className="journal-resolve-button" onClick={() => void resolveJournalEntry(entry)}>已人工恢复并重扫，标记已核对</Button>}
        </article>) : <div className="jobs-panel-empty">暂无资产文件操作记录</div>}</div></Modal>}
      {settingsOpen && <Modal opened onClose={() => { setSettingsOpen(false); }} title={<div><strong id="settings-title">外观与设置</strong></div>} size={620} zIndex={200} closeOnClickOutside={true} closeOnEscape={true} closeButtonProps={{ "aria-label": "关闭窗口" }} classNames={{ content: "library-modal", title: "library-modal-title", body: "settings-modal" }}><AppearanceSettings />{settingsError && <Alert color="red" role="alert" mb="sm">{settingsError}</Alert>}<div className="settings-field"><label>预览角色</label><p>选择 PMX 或 PMD 模型，用于动作与姿势预览，以及场景原点的尺寸参照。场景保留角色的原始坐标和尺寸。VMD 缩略图使用首帧或第一关键帧；包含镜头轨道时按镜头取景。</p><div className="settings-model-path" title={motionPreviewModel ?? "尚未设置"}>{motionPreviewModel ?? "尚未设置模型"}</div><div className="settings-modal-actions"><Button disabled={settingsBusy} onClick={() => void chooseMotionPreviewModel()}>{settingsBusy ? "正在保存…" : "选择模型"}</Button><Button disabled={settingsBusy || !motionPreviewModel} onClick={() => void clearMotionPreviewModel()}>清除</Button></div></div><div className="settings-field"><label>缩略图</label><p>重新生成在后台执行，可取消或重试。尚未设置预览模型时会跳过动作。</p><div className="settings-modal-actions"><Button disabled={settingsBusy} onClick={() => void regenerateAllThumbnails()}>{settingsBusy ? "正在加入队列…" : "重新生成全部缩略图"}</Button></div>{notice && <p role="status">{notice}</p>}</div><div className="settings-field concurrency-settings"><label>缩略图阶段并发上限</label><p>分别限制解析、GPU 渲染和 WebP 编码。自动模式会按设备资源选择；每阶段可设 1–8 路，渲染自动模式为 1 路。</p>
          {(["parse", "render", "encode"] as const).map((stage) => {
            const value = thumbnailConcurrencyDraft[stage];
            const selection = value === null ? "auto" : ([1, 2, 4, 8].includes(value) ? String(value) : "custom");
            const label = stage === "parse" ? "解析" : stage === "render" ? "渲染" : "编码";
            return <div className="concurrency-row" key={stage}><span>{label}</span><div className="concurrency-controls"><NativeSelect aria-label={`${label}并发上限`} value={selection} disabled={settingsBusy} onChange={(event) => {
              const selected = event.target.value;
              setThumbnailConcurrencyDraft((current) => ({ ...current, [stage]: selected === "auto" ? null : selected === "custom" ? (current[stage] ?? 3) : Number(selected) }));
            }}><option value="auto">自动</option><option value="1">1</option><option value="2">2</option><option value="4">4</option><option value="8">8</option><option value="custom">自定义</option></NativeSelect>
              {selection === "custom" && <TextInput aria-label={`${label}自定义并发数`} type="number" min={1} max={8} step={1} value={value ?? 3} disabled={settingsBusy} onChange={(event) => {
                const number = Number(event.target.value);
                if (Number.isInteger(number) && number >= 1 && number <= 8) setThumbnailConcurrencyDraft((current) => ({ ...current, [stage]: number }));
              }} />}</div></div>;
          })}
          <div className="settings-modal-actions"><Button disabled={settingsBusy} onClick={() => void saveThumbnailConcurrency()}>{settingsBusy ? "正在保存…" : "保存并发设置"}</Button></div>
        </div>{storageInfo && <div className="settings-field storage-settings"><label>便携数据库</label><p title={storageInfo.path}>{storageInfo.path}</p><div>数据库 {formatMiB(storageInfo.databaseBytes)} / {formatMiB(storageInfo.databaseLimitBytes)} · WAL {formatMiB(storageInfo.walBytes)}（目标不超过 {formatMiB(storageInfo.walTargetBytes)}）</div><div className="settings-modal-actions"><Button disabled={settingsBusy || activeScans.length > 0 || activeThumbnailCount > 0} onClick={() => void compactStorage()}>整理数据库和历史任务</Button></div></div>}<footer>模型文件保留在原位置；数据库保存在程序旁的 data 目录。</footer></Modal>}
      {addRootOpen && <Modal opened onClose={() => { if (!addRootBusy) setAddRootOpen(false); }} title={<div><strong id="add-root-title">添加目录并扫描</strong></div>} size={540} zIndex={200} closeOnClickOutside={!addRootBusy} closeOnEscape={!addRootBusy} closeButtonProps={{ "aria-label": "关闭窗口", disabled: addRootBusy }} classNames={{ content: "library-modal", title: "library-modal-title", body: "operation-modal add-root-modal" }}><form className="add-root-form" onSubmit={(event) => { event.preventDefault(); void submitAddRoot(); }}>
          <label className="add-root-field">资产类型<NativeSelect value={addRootType} disabled={addRootBusy} onChange={(event) => setAddRootType(event.target.value as AssetType)}><option value="model">模型</option><option value="motion">动作</option><option value="scene">场景</option></NativeSelect></label>
          <label className="add-root-field">目录路径<div className="add-root-path-row"><TextInput aria-label="目录路径" value={addRootPath} disabled={addRootBusy} placeholder="选择或输入本地目录路径" onChange={(event) => setAddRootPath(event.target.value)} /><Button type="button" disabled={addRootBusy} onClick={() => void chooseAddRootDirectory()}>选择目录</Button></div></label>
          <label className="add-root-field">显示名称（可选）<TextInput value={addRootName} maxLength={128} disabled={addRootBusy} placeholder="默认使用目录名称" onChange={(event) => setAddRootName(event.target.value)} /></label>
          <Checkbox className="add-root-recursive" checked={addRootRecursive} disabled={addRootBusy} onChange={(event) => setAddRootRecursive(event.target.checked)} label={<><span>递归扫描子目录</span></>} />
          {error && <div className="add-root-error" role="alert">{error}</div>}
          <footer className="operation-modal-actions"><Button type="button" disabled={addRootBusy} onClick={() => setAddRootOpen(false)}>取消</Button><Button variant="filled" className="primary" type="submit" disabled={addRootBusy}>{addRootBusy ? "正在添加并扫描…" : "添加并扫描"}</Button></footer>
        </form></Modal>}
    </AppShell>
  );
}
