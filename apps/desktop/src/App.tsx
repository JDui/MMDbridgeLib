import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { VirtuosoGrid } from "react-virtuoso";
import "./virtualized-grid.css";
import "./review-status.css";

const ModelViewer = lazy(() => import("./ModelViewer"));
const MotionViewer = lazy(() => import("./MotionViewer"));

function CardThumbnail({ assetId, alt }: { assetId: string; alt: string }) {
  const container = useRef<HTMLDivElement>(null);
  const [source, setSource] = useState("");
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    setSource("");
    setFailed(false);
    let active = true;
    let objectUrl = "";
    let requested = false;
    const load = () => {
      if (requested) return;
      requested = true;
      invoke<ArrayBuffer>("card_thumbnail", { assetId })
        .then((buffer) => {
          if (!active) return;
          if (!buffer?.byteLength) { setFailed(true); return; }
          objectUrl = URL.createObjectURL(new Blob([buffer], { type: "image/webp" }));
          setSource(objectUrl);
        })
        .catch(() => { if (active) setFailed(true); });
    };

    if (!("IntersectionObserver" in window)) {
      load();
      return () => { active = false; if (objectUrl) URL.revokeObjectURL(objectUrl); };
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        observer.disconnect();
        load();
      }
    }, { rootMargin: "240px" });
    observer.observe(element);
    return () => {
      active = false;
      observer.disconnect();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [assetId]);

  return <div className="card-thumbnail-slot" ref={container}>
    {source ? <img className="card-thumbnail-image" src={source} alt={alt} /> : failed ? <span className="thumbnail-load-error">缩略图读取失败</span> : null}
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
type AssetTag = { name: string; source: "user" | "agent" | "parser"; confidence: number | null };
type Job = { id: string; status: string; kind: string; progress: number; asset_id?: string | null; error?: { message?: string } | null };
type JobSummary = Record<string, number>;
type ScanState = { rootId: string; status: string; queueOrder: number; fullCheck: boolean; progress: number; filesSeen: number; filesProcessed: number; error: string | null; updatedAt: string };
type StorageInfo = { path: string; databaseBytes: number; walBytes: number; databaseLimitBytes: number; walTargetBytes: number };
type ThumbnailConcurrencySettings = { parse: number | null; render: number | null; encode: number | null };
type AssetOperationAsset = { id: string; name: string; primarySource: string };
type AssetOperationPlan = {
  operation: "move" | "rename" | "recycle";
  assetIds: string[];
  sourcePaths: string[];
  destinationPaths: string[];
  destinationParent: string | null;
  newName: string | null;
  affectedAssets: AssetOperationAsset[];
  dependencyPaths: string[];
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
  result: { message?: string } | null;
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
type AssetDuplicate = {
  id: string;
  assetA: string;
  assetAName: string;
  assetAPath: string;
  assetB: string;
  assetBName: string;
  assetBPath: string;
  similarity: number;
  reason: Record<string, unknown>;
};
type FilterField = "assetType" | "rootId" | "directory" | "tag" | "favorite" | "cardStatus" | "duplicateStatus" | "relationStatus" | "recentlyAdded" | "recentlyModified" | "needsReview" | "polygonCount" | "boneCount" | "hasThumbnail" | "hasCard" | "frameCount" | "duration" | "hasBoneMotion" | "hasMorphMotion" | "hasCamera" | "cameraOnly" | "pose" | "hasPairedCamera" | "fileType" | "width" | "depth" | "area";
type FilterOperator = "eq" | "ne" | "contains" | "gt" | "gte" | "lt" | "lte";
type FilterExpr =
  | { op: "and" | "or"; children: FilterExpr[] }
  | { op: "not"; child: FilterExpr }
  | { op: "rule"; field: FilterField; operator: FilterOperator; value: string | number | boolean };
type SavedFilter = { id: string; name: string; expression: FilterExpr; createdAt: string; updatedAt: string };
type BuilderRule = { field: FilterField; operator: FilterOperator; value: string; negate: boolean };

const categoryLabels: Record<AssetType, string> = { model: "模型", motion: "动作", scene: "场景" };
const categoryGlyphs: Record<AssetType, string> = { model: "◇", motion: "♫", scene: "▧" };
const filterFieldLabels: Record<FilterField, string> = {
  assetType: "资产类型", rootId: "资产根目录", directory: "目录", tag: "标签", favorite: "收藏",
  cardStatus: "资源卡状态", duplicateStatus: "重复项", relationStatus: "有关联", recentlyAdded: "添加时间",
  recentlyModified: "修改时间", needsReview: "需要复核", polygonCount: "面数", boneCount: "骨骼数",
  hasThumbnail: "有缩略图", hasCard: "有资源卡", frameCount: "动作帧数", duration: "动作时长",
  hasBoneMotion: "包含骨骼动作", hasMorphMotion: "包含表情动作", hasCamera: "包含镜头", cameraOnly: "纯镜头",
  pose: "Pose", hasPairedCamera: "有配套 Camera", fileType: "文件格式", width: "场景宽度", depth: "场景深度", area: "场景面积",
};
const booleanFilterFields = new Set<FilterField>(["favorite", "duplicateStatus", "relationStatus", "needsReview", "hasThumbnail", "hasCard", "hasBoneMotion", "hasMorphMotion", "hasCamera", "cameraOnly", "pose", "hasPairedCamera"]);
const numericFilterFields = new Set<FilterField>(["polygonCount", "boneCount", "frameCount", "duration", "width", "depth", "area"]);
const dateFilterFields = new Set<FilterField>(["recentlyAdded", "recentlyModified"]);
const metadataLabels: Record<string, string> = {
  polygon_count: "面数", vertex_count: "顶点数", bone_count: "骨骼数", material_count: "材质数",
  morph_count: "Morph 数", rigid_body_count: "刚体数", joint_count: "关节数", total_frames: "总帧数",
  duration_seconds: "时长（秒）", start_frame: "起始帧", end_frame: "结束帧", has_bone_motion: "骨骼动画",
  has_morph_motion: "表情动画", has_camera: "包含镜头", has_light: "包含灯光", is_camera_only: "纯镜头",
  is_pose: "Pose", width: "宽度（MMD 单位）", depth: "深度（MMD 单位）", area: "占地面积",
  file_type: "格式", pmx_version: "PMX 版本", preview_frame: "预览帧",
};

function formatValue(value: unknown): string {
  if (typeof value === "boolean") return value ? "是" : "否";
  if (typeof value === "number") return Number.isInteger(value) ? value.toLocaleString("zh-CN") : value.toFixed(2);
  return String(value);
}

function formatMiB(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function reviewReason(asset: Asset): string {
  const reasons: string[] = [];
  if (asset.metadata.candidate_reason === "multiple_primary_models_in_directory") reasons.push("同一目录中有多个主模型，需确认应使用哪一个");
  if (asset.metadata.card_identity_ambiguous === true) reasons.push("附近有无法明确归属的资源卡");
  const dependencies = Array.isArray(asset.metadata.file_dependencies) ? asset.metadata.file_dependencies : [];
  const missing = dependencies.filter((item) => item && typeof item === "object" && "status" in item && item.status === "missing").length;
  const external = dependencies.filter((item) => item && typeof item === "object" && "status" in item && item.status === "external").length;
  if (missing) reasons.push(`${missing} 个贴图或依赖文件未找到`);
  if (external) reasons.push(`${external} 个贴图或依赖文件位于资产包外`);
  const diagnostics = Array.isArray(asset.metadata.parser_diagnostics) ? asset.metadata.parser_diagnostics.length : 0;
  if (diagnostics) reasons.push(`解析器记录了 ${diagnostics} 条提示`);
  return reasons.join("；");
}

function assetStatusText(statuses: string[]): string {
  return statuses.filter((status) => status !== "NeedsReview").map((status) => ({ Ready: "就绪", ParseFailed: "解析失败", MissingSource: "源文件缺失", Unsupported: "暂不支持" } as Record<string, string>)[status] ?? status).join(" · ") || "已索引";
}

function isAssetType(value: string): value is AssetType {
  return value === "model" || value === "motion" || value === "scene";
}

function operatorsFor(field: FilterField): FilterOperator[] {
  if (booleanFilterFields.has(field)) return ["eq", "ne"];
  if (numericFilterFields.has(field) || dateFilterFields.has(field)) return ["eq", "ne", "gt", "gte", "lt", "lte"];
  return ["eq", "ne", "contains"];
}

const operatorLabels: Record<FilterOperator, string> = {
  eq: "等于", ne: "不等于", contains: "包含", gt: "大于", gte: "至少", lt: "小于", lte: "至多",
};
const LIBRARY_PAGE_SIZE = 120;
const activeScanStatuses = new Set(["Pending", "Discovering", "Indexing", "Verifying", "Relations", "Duplicates", "Pausing", "Cancelling"]);
const scanStatusLabels: Record<string, string> = {
  Pending: "排队中", Discovering: "发现文件", Indexing: "建立索引", Verifying: "检查资源卡",
  Relations: "分析关系", Duplicates: "分析重复项", Pausing: "正在暂停", Paused: "已暂停",
  Cancelling: "正在停止", Completed: "已完成", Failed: "失败", Cancelled: "已停止",
};

export default function App() {
  const [roots, setRoots] = useState<Root[]>([]);
  const [assets, setAssets] = useState<Asset[]>([]);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [jobSummary, setJobSummary] = useState<JobSummary>({});
  const [scanStates, setScanStates] = useState<ScanState[]>([]);
  const [scanQueueOpen, setScanQueueOpen] = useState(false);
  const [previewAsset, setPreviewAsset] = useState<Asset | null>(null);
  const [cardSize, setCardSize] = useState(() => {
    const saved = Number(window.localStorage.getItem("mmdbridge-card-size"));
    return Number.isFinite(saved) && saved >= 130 && saved <= 300 ? saved : 176;
  });
  const [storageInfo, setStorageInfo] = useState<StorageInfo | null>(null);
  const [assetTags, setAssetTags] = useState<AssetTag[]>([]);
  const [assetRelations, setAssetRelations] = useState<AssetRelation[]>([]);
  const [assetDuplicates, setAssetDuplicates] = useState<AssetDuplicate[]>([]);
  const [nextAssetCursor, setNextAssetCursor] = useState<AssetCursor | null>(null);
  const [loadingNextPage, setLoadingNextPage] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [savedFilters, setSavedFilters] = useState<SavedFilter[]>([]);
  const [counts, setCounts] = useState<AssetCounts>({ all: 0, model: 0, motion: 0, scene: 0, byRoot: {} });
  const [activeType, setActiveType] = useState<AssetType | "all">("all");
  const [activeMotionFormat, setActiveMotionFormat] = useState<MotionFormat>("all");
  const [activeRoot, setActiveRoot] = useState<string | null>(null);
  const [activeDirectory, setActiveDirectory] = useState<string | null>(null);
  const [assetDirectories, setAssetDirectories] = useState<AssetDirectory[]>([]);
  useEffect(() => { setActiveDirectory(null); }, [activeRoot]);
  const [activeSavedFilterId, setActiveSavedFilterId] = useState<string | null>(null);
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [duplicatesOnly, setDuplicatesOnly] = useState(false);
  const [duplicateCount, setDuplicateCount] = useState(0);
  const [filterBuilderOpen, setFilterBuilderOpen] = useState(false);
  const [filterName, setFilterName] = useState("");
  const [filterGroupOp, setFilterGroupOp] = useState<"and" | "or">("and");
  const [filterRules, setFilterRules] = useState<BuilderRule[]>([{ field: "assetType", operator: "eq", value: "motion", negate: false }]);
  const [selected, setSelected] = useState<Asset | null>(null);
  const [assetMenu, setAssetMenu] = useState<{ asset: Asset; x: number; y: number } | null>(null);
  const assetMenuRef = useRef<HTMLDivElement>(null);
  const [bulkSelectMode, setBulkSelectMode] = useState(false);
  const [bulkSelectedIds, setBulkSelectedIds] = useState<Set<string>>(() => new Set());
  const [viewerAsset, setViewerAsset] = useState<{ id?: string; name: string; primarySource: string; assetType?: "model" | "scene" } | null>(null);
  const [motionViewerAsset, setMotionViewerAsset] = useState<Asset | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [motionPreviewModel, setMotionPreviewModel] = useState<string | null>(null);
  const [thumbnailConcurrencyDraft, setThumbnailConcurrencyDraft] = useState<ThumbnailConcurrencySettings>({ parse: null, render: null, encode: null });
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [assetOperationPlan, setAssetOperationPlan] = useState<AssetOperationPlan | null>(null);
  const [operationJournalOpen, setOperationJournalOpen] = useState(false);
  const [operationJournal, setOperationJournal] = useState<AssetOperationJournalEntry[]>([]);
  const [libraryScrollParent, setLibraryScrollParent] = useState<HTMLDivElement | null>(null);
  const [query, setQuery] = useState("");
  const [searchText, setSearchText] = useState("");
  const [busy, setBusy] = useState(false);
  const [jobsExpanded, setJobsExpanded] = useState(false);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const assetQueryRevision = useRef(0);
  const assetPageLoading = useRef<number | null>(null);
  const scanStatesRef = useRef<ScanState[]>([]);

  useEffect(() => { window.localStorage.setItem("mmdbridge-card-size", String(cardSize)); }, [cardSize]);

  useEffect(() => {
    if (!assetMenu) return;
    const dismiss = (event: PointerEvent) => {
      if (!assetMenuRef.current?.contains(event.target as Node)) setAssetMenu(null);
    };
    const close = () => setAssetMenu(null);
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") close(); };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [assetMenu]);

  function openAssetMenu(event: React.MouseEvent<HTMLElement>, asset: Asset) {
    event.preventDefault();
    event.stopPropagation();
    setSelected(asset);
    setAssetMenu({
      asset,
      x: Math.max(8, Math.min(event.clientX, window.innerWidth - 232)),
      y: Math.max(8, Math.min(event.clientY, window.innerHeight - 238)),
    });
  }

  useEffect(() => {
    setBulkSelectedIds(new Set());
  }, [activeRoot, activeDirectory, activeSavedFilterId, activeType, activeMotionFormat, duplicatesOnly, favoritesOnly, searchText]);

  const refresh = useCallback(async () => {
    const revision = ++assetQueryRevision.current;
    setIsRefreshing(true);
    assetPageLoading.current = null;
    setLoadingNextPage(false);
    setNextAssetCursor(null);
    try {
      const assetPageArgs = {
        assetType: activeType === "all" ? null : activeType,
        query: searchText || null,
        rootId: activeRoot,
        cursor: null,
        limit: LIBRARY_PAGE_SIZE,
      };
      const assetRequest = duplicatesOnly
        ? invoke<AssetPage>("duplicate_assets_page", assetPageArgs)
        : invoke<AssetPage>("assets_page", {
          assetType: activeType === "all" ? null : activeType,
          motionFormat: activeType === "motion" && activeMotionFormat !== "all" ? activeMotionFormat : null,
          query: searchText || null,
          rootId: activeRoot,
          directoryPath: activeDirectory,
          favoritesOnly,
          filterId: activeSavedFilterId,
          cursor: null,
          limit: LIBRARY_PAGE_SIZE,
        });
      const current = () => revision === assetQueryRevision.current;
      const reportError = (reason: unknown) => { if (current()) setError(String(reason)); };
      await Promise.allSettled([
        invoke<Root[]>("roots_list").then((value) => { if (current()) setRoots(value); }).catch(reportError),
        invoke<AssetCounts>("asset_counts").then((value) => { if (current()) setCounts(value); }).catch(reportError),
        activeRoot ? invoke<AssetDirectory[]>("asset_directories", { rootId: activeRoot }).then((value) => { if (current()) setAssetDirectories(value); }).catch(reportError) : Promise.resolve().then(() => { if (current()) setAssetDirectories([]); }),
        assetRequest.then((page) => {
          if (!current()) return;
          const visible = page.items.filter((asset) => !activeRoot || asset.rootId === activeRoot);
          setAssets(visible);
          setNextAssetCursor(page.nextCursor);
          setSelected((previous) => visible.find((asset) => asset.id === previous?.id) ?? null);
          setIsRefreshing(false);
        }).catch(reportError),
        invoke<Job[]>("jobs_list").then((value) => { if (current()) setJobs(value); }).catch(reportError),
        invoke<JobSummary>("jobs_summary").then((value) => { if (current()) setJobSummary(value); }).catch(reportError),
        invoke<ScanState[]>("scan_states").then((value) => {
          if (!current()) return;
          scanStatesRef.current = value;
          setScanStates(value);
        }).catch(reportError),
        invoke<number>("duplicates_count").then((value) => { if (current()) setDuplicateCount(value); }).catch(reportError),
        invoke<SavedFilter[]>("filters_list").then((value) => { if (current()) setSavedFilters(value); }).catch(reportError),
      ]);
    } catch (reason) {
      if (revision === assetQueryRevision.current) setError(String(reason));
    } finally {
      if (revision === assetQueryRevision.current) setIsRefreshing(false);
    }
  }, [activeRoot, activeDirectory, activeSavedFilterId, activeType, activeMotionFormat, duplicatesOnly, favoritesOnly, searchText]);

  const loadNextAssetPage = useCallback(async () => {
    const cursor = nextAssetCursor;
    const revision = assetQueryRevision.current;
    if (!cursor || assetPageLoading.current === revision) return;
    assetPageLoading.current = revision;
    setLoadingNextPage(true);
    try {
      const page = duplicatesOnly
        ? await invoke<AssetPage>("duplicate_assets_page", {
          assetType: activeType === "all" ? null : activeType,
          query: searchText || null,
          rootId: activeRoot,
          cursor,
          limit: LIBRARY_PAGE_SIZE,
        })
        : await invoke<AssetPage>("assets_page", {
          assetType: activeType === "all" ? null : activeType,
          motionFormat: activeType === "motion" && activeMotionFormat !== "all" ? activeMotionFormat : null,
          query: searchText || null,
          rootId: activeRoot,
          directoryPath: activeDirectory,
          favoritesOnly,
          filterId: activeSavedFilterId,
          cursor,
          limit: LIBRARY_PAGE_SIZE,
        });
      if (revision !== assetQueryRevision.current) return;
      setAssets((current) => {
        const existingIds = new Set(current.map((asset) => asset.id));
        return [...current, ...page.items.filter((asset) => !existingIds.has(asset.id))];
      });
      setNextAssetCursor(page.nextCursor);
    } catch (reason) {
      if (revision === assetQueryRevision.current) setError(String(reason));
    } finally {
      if (assetPageLoading.current === revision) assetPageLoading.current = null;
      if (revision === assetQueryRevision.current) setLoadingNextPage(false);
    }
  }, [activeRoot, activeDirectory, activeSavedFilterId, activeType, activeMotionFormat, duplicatesOnly, favoritesOnly, nextAssetCursor, searchText]);

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
      .catch((reason) => { if (!disposed) setError(String(reason)); });
    return () => {
      disposed = true;
      unlisten.forEach((stop) => stop());
    };
  }, [refresh]);

  useEffect(() => {
    const active = jobs.some((job) => ["Pending", "Parsing", "Rendering", "Encoding"].includes(job.status));
    if (!active) return;
    let disposed = false;
    const timer = window.setInterval(() => {
      void Promise.all([invoke<Job[]>("jobs_list"), invoke<JobSummary>("jobs_summary")])
        .then(([nextJobs, nextSummary]) => {
          if (disposed) return;
          setJobs(nextJobs);
          setJobSummary(nextSummary);
          if (!nextJobs.some((job) => ["Pending", "Parsing", "Rendering", "Encoding"].includes(job.status))) {
            void refresh();
          }
        })
        .catch((reason) => { if (!disposed) setError(String(reason)); });
    }, 700);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [jobs, refresh]);

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
      }).catch((reason) => { if (!disposed) setError(String(reason)); });
    }, 1200);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [refresh]);

  useEffect(() => {
    if (!selected) {
      setAssetTags([]);
      setAssetRelations([]);
      setAssetDuplicates([]);
      return;
    }
    let active = true;
    Promise.all([
      invoke<AssetTag[]>("asset_tags", { assetId: selected.id }),
      invoke<AssetRelation[]>("relations_list", { assetId: selected.id, limit: 100 }),
      invoke<AssetDuplicate[]>("duplicates_list", { assetId: selected.id, limit: 100 }),
    ])
      .then(([tags, relations, duplicates]) => {
        if (active) {
          setAssetTags(tags);
          setAssetRelations(relations);
          setAssetDuplicates(duplicates);
        }
      })
      .catch((reason) => { if (active) setError(String(reason)); });
    return () => { active = false; };
  }, [selected?.id]);

  const activeJobs = useMemo(
    () => jobs.filter((job) => ["Pending", "Parsing", "Rendering", "Encoding"].includes(job.status)),
    [jobs],
  );
  const activeScans = useMemo(() => scanStates.filter((scan) => activeScanStatuses.has(scan.status)), [scanStates]);
  const activeThumbnailCount = (jobSummary.Pending ?? 0) + (jobSummary.Parsing ?? 0)
    + (jobSummary.Rendering ?? 0) + (jobSummary.Encoding ?? 0);
  const totalThumbnailCount = Object.values(jobSummary).reduce((total, count) => total + count, 0);

  function selectCategory(type: AssetType | "all") {
    setFavoritesOnly(false);
    setDuplicatesOnly(false);
    setActiveSavedFilterId(null);
    setActiveType(type);
    setActiveRoot(null);
    setActiveDirectory(null);
    setSelected(null);
  }

  function selectMotionFormat(format: MotionFormat) {
    setActiveMotionFormat(format);
    setAssets([]);
    setSelected(null);
    setNextAssetCursor(null);
  }

  async function addRoot(type: AssetType) {
    const path = await open({ directory: true, multiple: false, title: `添加${categoryLabels[type]}目录` });
    if (typeof path !== "string" || !path.trim()) return;
    setBusy(true);
    setNotice("正在添加资产库…");
    try {
      const root = await invoke<Root>("root_add", { assetType: type, path: path.trim(), name: null });
      setActiveType(type);
      setActiveRoot(root.id);
      setActiveSavedFilterId(null);
      setFavoritesOnly(false);
      setDuplicatesOnly(false);
      await refresh();
      setNotice("已添加。可点击目录旁的扫描按钮建立索引。");
    } catch (reason) {
      setError(String(reason));
      setNotice("");
    } finally {
      setBusy(false);
    }
  }

  async function openModelPreview() {
    const path = await open({
      multiple: false,
      title: "打开 PMX 模型预览",
      filters: [{ name: "PMX 模型", extensions: ["pmx"] }],
    });
    if (typeof path !== "string" || !path.trim()) return;
    const name = path.split(/[\\/]/).pop() ?? path;
    setViewerAsset({ name, primarySource: path });
  }

  async function openSettings() {
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
      setError(String(reason));
    }
  }

  async function chooseMotionPreviewModel() {
    const path = await open({
      multiple: false,
      title: "选择 Motion Preview Model",
      filters: [{ name: "PMX 模型", extensions: ["pmx"] }],
    });
    if (typeof path !== "string" || !path.trim()) return;
    setSettingsBusy(true);
    try {
      const saved = await invoke<string | null>("motion_preview_model_set", { path });
      setMotionPreviewModel(saved);
      setNotice("已保存动作预览模型。VMD/VPD 资源卡会使用该模型生成动作或姿势缩略图。");
      setError("");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSettingsBusy(false);
    }
  }

  async function clearMotionPreviewModel() {
    setSettingsBusy(true);
    try {
      await invoke<string | null>("motion_preview_model_set", { path: null });
      setMotionPreviewModel(null);
      setNotice("已清除动作预览模型设置。");
      setError("");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSettingsBusy(false);
    }
  }

  async function saveThumbnailConcurrency() {
    setSettingsBusy(true);
    try {
      const saved = await invoke<ThumbnailConcurrencySettings>("thumbnail_concurrency_set", { settings: thumbnailConcurrencyDraft });
      setThumbnailConcurrencyDraft(saved);
      setNotice("已保存缩略图并发设置，新设置立即生效。");
      setError("");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSettingsBusy(false);
    }
  }

  async function compactStorage() {
    setSettingsBusy(true);
    try {
      const storage = await invoke<StorageInfo>("storage_compact");
      setStorageInfo(storage);
      setNotice("数据库整理完成，已保留最近 1000 条任务记录。");
      setError("");
    } catch (reason) { setError(String(reason)); }
    finally { setSettingsBusy(false); }
  }

  async function requestAssetOperation(
    operation: AssetOperationPlan["operation"],
    assetIds: string[],
    destinationParent: string | null = null,
    newName: string | null = null,
  ) {
    setBusy(true);
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
      setError(String(reason));
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
    const newName = window.prompt("重命名整个资产包文件夹", currentName);
    if (!newName?.trim()) return;
    await requestAssetOperation("rename", [asset.id], null, newName.trim());
  }

  async function openOperationJournal() {
    try {
      const entries = await invoke<AssetOperationJournalEntry[]>("operation_journal_list", { limit: 100 });
      setOperationJournal(entries);
      setOperationJournalOpen(true);
      setError("");
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function resolveJournalEntry(entry: AssetOperationJournalEntry) {
    if (!window.confirm("请先在资源管理器检查源路径与目标路径，并恢复文件或重新扫描相关资产。确认已完成这些人工核对后，才标记此记录已处理。")) return;
    try {
      await invoke<boolean>("operation_journal_resolve", { operationId: entry.id });
      setOperationJournal(await invoke<AssetOperationJournalEntry[]>("operation_journal_list", { limit: 100 }));
      await refresh();
      setNotice("恢复记录已标记为已核对。");
      setError("");
    } catch (reason) { setError(String(reason)); }
  }

  async function executePlannedAssetOperation() {
    if (!assetOperationPlan?.canExecute) return;
    setBusy(true);
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
        setOperationJournal(await invoke<AssetOperationJournalEntry[]>("operation_journal_list", { limit: 100 }));
      }
    } catch (reason) {
      setError(String(reason));
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
      setError(String(reason));
      setNotice("");
    }
  }

  async function openAssetDirectory(asset: Asset) {
    try {
      await invoke("asset_open_directory", { assetId: asset.id });
      setNotice("已打开资产源目录。");
      setError("");
    } catch (reason) {
      setError(String(reason));
      setNotice("");
    }
  }

  function addAnyRoot() {
    const value = window.prompt("选择资产类型：model / motion / scene，也可输入 模型 / 动作 / 场景");
    if (!value?.trim()) return;
    const aliases: Record<string, AssetType> = { 模型: "model", 动作: "motion", 场景: "scene", stage: "scene" };
    const normalized = value.trim().toLowerCase();
    const type = aliases[normalized] ?? (isAssetType(normalized) ? normalized : null);
    if (type) void addRoot(type);
    else setError("请输入 model、motion、scene 或对应的中文类型。");
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
      setError(String(reason));
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
    } catch (reason) { setError(String(reason)); }
  }

  async function pauseScan(rootId: string) {
    try {
      await invoke("scan_pause", { rootId });
      const states = await invoke<ScanState[]>("scan_states");
      scanStatesRef.current = states;
      setScanStates(states);
    } catch (reason) { setError(String(reason)); }
  }

  async function moveScan(rootId: string, direction: -1 | 1) {
    try {
      await invoke("scan_move", { rootId, direction });
      const states = await invoke<ScanState[]>("scan_states");
      scanStatesRef.current = states;
      setScanStates(states);
    } catch (reason) { setError(String(reason)); }
  }

  async function queueCards(root: Root) {
    setNotice(`正在为 ${root.displayName} 安排软件生成资源卡…`);
    try {
      const count = await invoke<number>("cards_queue_root", { rootId: root.id });
      await refresh();
      setNotice(`${root.displayName} 已加入 ${count} 个资源卡生成任务。`);
    } catch (reason) { setError(String(reason)); setNotice(""); }
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
      setNotice(changes.displayName !== undefined ? "已更新目录名称。" : changes.enabled !== undefined ? (updated.enabled ? "已恢复目录扫描。" : "已暂停目录扫描。") : "已更新递归扫描设置。");
    } catch (reason) { setError(String(reason)); }
  }

  function renameRoot(root: Root) {
    const displayName = window.prompt("目录显示名称", root.displayName);
    if (displayName === null || displayName.trim() === "" || displayName.trim() === root.displayName) return;
    void updateRoot(root, { displayName: displayName.trim() });
  }

  async function removeRoot(root: Root) {
    if (!window.confirm(`从 Library 移除“${root.displayName}”？磁盘文件不会删除。`)) return;
    try {
      await invoke("root_remove", { rootId: root.id });
      setActiveRoot(null);
      setSelected(null);
      await refresh();
      setNotice("已从 Library 移除目录索引。");
    } catch (reason) { setError(String(reason)); }
  }

  async function createCard(assetId: string) {
    setBusy(true);
    setNotice("正在写入资源卡…");
    try {
      const result = await invoke<{ id?: string; status: string; cardPath?: string; message?: string }>("card_create", { assetId });
      await refresh();
      setNotice(result.id ? "缩略图任务已加入队列。" : result.message ?? `资源卡已写入：${result.cardPath}`);
    } catch (reason) {
      setError(String(reason));
      setNotice("");
    } finally {
      setBusy(false);
    }
  }

  async function cancelJob(jobId: string) {
    try {
      await invoke("jobs_cancel", { jobId });
      await refresh();
      setNotice("已取消缩略图任务。");
    } catch (reason) { setError(String(reason)); }
  }

  async function retryJob(jobId: string) {
    try {
      await invoke("jobs_retry", { jobId });
      await refresh();
      setNotice("已重新加入缩略图队列。");
    } catch (reason) { setError(String(reason)); }
  }

  async function verifyCard(assetId: string) {
    try {
      const result = await invoke<{ status: string; message?: string }>("card_verify", { assetId });
      await refresh();
      setNotice(result.message ?? `资源卡校验结果：${result.status}`);
    } catch (reason) { setError(String(reason)); }
  }

  async function addTag() {
    if (!selected) return;
    const assetId = selected.id;
    const name = window.prompt("添加标签");
    if (!name?.trim()) return;
    try {
      const result = await invoke<{ blockedByUser: boolean }>("tag_add", { assetId, name: name.trim(), source: "user" });
      setAssetTags(await invoke<AssetTag[]>("asset_tags", { assetId }));
      await refresh();
      setNotice(result.blockedByUser ? "此标签已被手动移除，自动标签来源不会重新添加它。" : "已保存用户标签。");
    } catch (reason) { setError(String(reason)); }
  }

  async function addTagToSelection() {
    const assetIds = Array.from(bulkSelectedIds);
    if (!assetIds.length) return;
    const name = window.prompt(`为 ${assetIds.length} 项资产添加用户标签`);
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
      const selectedId = selected && bulkSelectedIds.has(selected.id) ? selected.id : null;
      setBulkSelectedIds(new Set());
      setBulkSelectMode(false);
      setNotice(`批量标签已完成：更新 ${changed}/${mutations.length} 项，受手动移除规则阻止 ${blocked} 项。`);
      if (selectedId) {
        try { setAssetTags(await invoke<AssetTag[]>("asset_tags", { assetId: selectedId })); }
        catch (reason) { setError(`详情标签刷新失败：${String(reason)}`); }
      }
      await refresh();
    } catch (reason) {
      setError(`批量标签失败：${String(reason)}`);
    } finally {
      setBusy(false);
    }
  }

  async function removeTagFromSelection() {
    const assetIds = Array.from(bulkSelectedIds);
    if (!assetIds.length) return;
    const name = window.prompt(`从 ${assetIds.length} 项资产移除哪个标签？手动移除会阻止后续自动标签重新添加。`);
    if (!name?.trim()) return;

    setBusy(true);
    try {
      const mutations = await invoke<Array<{ changed: boolean }>>("tag_remove_batch", {
        assetIds,
        name: name.trim(),
      });
      const changed = mutations.filter((mutation) => mutation.changed).length;
      const selectedId = selected && bulkSelectedIds.has(selected.id) ? selected.id : null;
      setBulkSelectedIds(new Set());
      setBulkSelectMode(false);
      setNotice(`批量移除标签完成：移除 ${changed}/${mutations.length} 项，已为所选资产记录手动移除规则。`);
      if (selectedId) {
        try { setAssetTags(await invoke<AssetTag[]>("asset_tags", { assetId: selectedId })); }
        catch (reason) { setError(`详情标签刷新失败：${String(reason)}`); }
      }
      await refresh();
    } catch (reason) {
      setError(`批量移除标签失败：${String(reason)}`);
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
      setError(`批量${favorite ? "收藏" : "取消收藏"}失败：${String(reason)}`);
    } finally {
      setBusy(false);
    }
  }

  function toggleBulkSelection(asset: Asset) {
    if (!bulkSelectMode) {
      setSelected(asset);
      return;
    }
    setBulkSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(asset.id)) next.delete(asset.id);
      else next.add(asset.id);
      return next;
    });
  }

  async function removeTag(name: string) {
    if (!selected) return;
    const assetId = selected.id;
    try {
      await invoke("tag_remove", { assetId, name });
      setAssetTags(await invoke<AssetTag[]>("asset_tags", { assetId }));
      await refresh();
      setNotice(`已移除标签“${name}”。`);
    } catch (reason) { setError(String(reason)); }
  }

  async function toggleFavorite(asset: Asset) {
    try {
      await invoke("favorite_set", { assetId: asset.id, favorite: !asset.isFavorite });
      await refresh();
    } catch (reason) { setError(String(reason)); }
  }

  async function confirmRelation(relation: AssetRelation) {
    try {
      await invoke("relation_confirm", { relationId: relation.id });
      setAssetRelations((current) => current.map((item) => item.id === relation.id ? { ...item, confirmed: true } : item));
      setNotice("已确认这条关系建议。");
    } catch (reason) { setError(String(reason)); }
  }

  function changeFilterRule(index: number, updates: Partial<BuilderRule>) {
    setFilterRules((current) => current.map((rule, ruleIndex) => ruleIndex === index ? { ...rule, ...updates } : rule));
  }

  function changeFilterField(index: number, field: FilterField) {
    const value = booleanFilterFields.has(field) ? "true" : numericFilterFields.has(field) ? "0" : field === "assetType" ? "motion" : "";
    const operator: FilterOperator = numericFilterFields.has(field) || dateFilterFields.has(field) ? "gte" : ["tag", "directory"].includes(field) ? "contains" : "eq";
    changeFilterRule(index, { field, operator, value });
  }

  function selectSavedFilter(filter: SavedFilter) {
    setFavoritesOnly(false);
    setDuplicatesOnly(false);
    setActiveSavedFilterId(filter.id);
    setActiveType("all");
    setActiveRoot(null);
    setSelected(null);
    setFilterBuilderOpen(false);
    setQuery("");
    setSearchText("");
  }

  async function saveSmartFilter() {
    if (!filterName.trim()) {
      setError("请为智能集合输入名称。");
      return;
    }
    try {
      const children: FilterExpr[] = filterRules.map((rule) => {
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
      setDuplicatesOnly(false);
      setActiveType("all");
      setActiveRoot(null);
      setSelected(null);
      setActiveSavedFilterId(saved.id);
      setFilterBuilderOpen(false);
      setQuery("");
      setSearchText("");
      setNotice(`已保存并应用智能集合“${saved.name}”。`);
      setError("");
    } catch (reason) { setError(String(reason)); }
  }

  async function removeSmartFilter(filter: SavedFilter) {
    if (!window.confirm(`删除智能集合“${filter.name}”？资产文件不会受到影响。`)) return;
    try {
      await invoke("filter_remove", { filterId: filter.id });
      if (activeSavedFilterId === filter.id) setActiveSavedFilterId(null);
      else await refresh();
    } catch (reason) { setError(String(reason)); }
  }

  function submitSearch(event: React.FormEvent) {
    event.preventDefault();
    setSearchText(query.trim());
  }

  const visibleAssets = assets.filter((asset) => (activeType === "all" || asset.assetType === activeType)
    && (!activeRoot || asset.rootId === activeRoot));
  const folderRoot = roots.find((root) => root.id === activeRoot);
  const folderBase = activeDirectory ?? folderRoot?.path ?? null;
  const folderPrefix = folderBase ? `${folderBase.replace(/[\\/]+$/, "")}\\` : "";
  const childFolders = new Map<string, { path: string; count: number }>();
  if (folderRoot && folderBase) for (const directory of assetDirectories) {
    if (!directory.path.toLowerCase().startsWith(folderPrefix.toLowerCase())) continue;
    const segment = directory.path.slice(folderPrefix.length).split(/[\\/]/)[0];
    if (!segment) continue;
    const path = `${folderPrefix}${segment}`;
    const previous = childFolders.get(path) ?? { path, count: 0 };
    previous.count += directory.count;
    childFolders.set(path, previous);
  }
  const indexedTotal = activeDirectory
    ? assetDirectories.filter((directory) => directory.path.toLowerCase() === activeDirectory.toLowerCase()
      || directory.path.toLowerCase().startsWith(`${activeDirectory.toLowerCase()}\\`)).reduce((total, directory) => total + directory.count, 0)
    : activeRoot ? counts.byRoot[activeRoot] ?? 0 : activeType === "all" ? counts.all : counts[activeType];
  const showIndexedTotal = !searchText && !favoritesOnly && !duplicatesOnly && !activeSavedFilterId && (activeType !== "motion" || activeMotionFormat === "all");
  const activeTitle = duplicatesOnly ? "重复项" : favoritesOnly ? "收藏" : savedFilters.find((filter) => filter.id === activeSavedFilterId)?.name ?? (activeRoot ? roots.find((root) => root.id === activeRoot)?.displayName ?? "Library" : activeType === "all" ? "全部资产" : categoryLabels[activeType]);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-row">
          <div className="brand-mark"><span /><span /><span /></div>
          <div><div className="brand-name">MMDbridge<span>Lib</span></div><div className="brand-subtitle">ASSET LIBRARY</div></div>
        </div>

        <button className={`nav-item library-home ${activeType === "all" && !activeRoot ? "active" : ""}`} onClick={() => selectCategory("all")}>
          <span className="nav-icon">▦</span><span>Library</span><span className="count">{counts.all}</span>
        </button>
        <div className="sidebar-section-heading"><span>资产类型</span><button aria-label="添加资产根目录" className="icon-button tiny" onClick={addAnyRoot}>＋</button></div>
        {(Object.keys(categoryLabels) as AssetType[]).map((type) => (
          <div className={`type-block ${activeType === type ? "type-selected" : ""}`} key={type}>
            <button className="nav-item type-item" onClick={() => selectCategory(type)}>
              <span className={`type-icon ${type}`}>{categoryGlyphs[type]}</span><span>{categoryLabels[type]}</span><span className="count">{counts[type]}</span>
            </button>
            {(activeType === type || activeType === "all") && <div className="root-list">
              {roots.filter((root) => root.assetType === type).map((root) => (
                <div className={`root-row ${activeRoot === root.id ? "selected" : ""}`} key={root.id}>
                  <button className="root-name" title={root.path} onClick={() => { setActiveType(type); setActiveRoot(root.id); setActiveSavedFilterId(null); setFavoritesOnly(false); setDuplicatesOnly(false); setSelected(null); }}>
                    <span className={`root-dot ${root.enabled ? "" : "paused"}`} /><span className="root-label">{root.displayName}</span><span className="count">{counts.byRoot[root.id] ?? 0}</span>
                  </button>
                  {scanStates.find((scan) => scan.rootId === root.id && activeScanStatuses.has(scan.status)) &&
                    <span className="root-scan-percent" title="扫描进度">{Math.round((scanStates.find((scan) => scan.rootId === root.id)?.progress ?? 0) * 100)}%</span>}
                  <button className="root-action" title="修改显示名称" disabled={!!activeScans.length} onClick={() => renameRoot(root)}>✎</button>
                  <button className="root-action" title="扫描目录" disabled={busy || !root.enabled} onClick={() => void scanRoot(root)}>↻</button>
                  <button className="root-action" title="完整检查源文件内容及资源卡版本" disabled={busy || !root.enabled || scanStates.some((scan) => scan.rootId === root.id && (activeScanStatuses.has(scan.status) || scan.status === "Paused"))} onClick={() => void scanRoot(root, true)}>✓</button>
                  <button className="root-action" title="批量生成本目录资源卡和缩略图" disabled={busy || !root.enabled || activeScans.some((scan) => scan.rootId === root.id)} onClick={() => void queueCards(root)}>▧</button>
                  <button className="root-action remove" title="从 Library 移除" disabled={!!activeScans.length} onClick={() => void removeRoot(root)}>×</button>
                </div>
              ))}
              <button className="add-root" onClick={() => void addRoot(type)}><span>＋</span> 添加目录</button>
            </div>}
          </div>
        ))}

        <div className="sidebar-divider" />
        <button className={`nav-item subdued ${favoritesOnly ? "active" : ""}`} onClick={() => { setFavoritesOnly(true); setDuplicatesOnly(false); setActiveSavedFilterId(null); setActiveType("all"); setActiveRoot(null); setSelected(null); setQuery(""); setSearchText(""); }}><span className="nav-icon">◇</span><span>收藏</span><span className="count">{favoritesOnly ? assets.length : ""}</span></button>
        <button className={`nav-item subdued ${duplicatesOnly ? "active" : ""}`} onClick={() => { setFavoritesOnly(false); setDuplicatesOnly(true); setActiveSavedFilterId(null); setActiveType("all"); setActiveRoot(null); setSelected(null); setQuery(""); setSearchText(""); }}><span className="nav-icon">⧉</span><span>重复项</span><span className="count">{duplicateCount}</span></button>
        <div className="sidebar-section-heading smart-filter-heading"><span>智能集合</span><button aria-label="新建智能集合" className="icon-button tiny" onClick={() => setFilterBuilderOpen((open) => !open)}>＋</button></div>
        {savedFilters.map((filter) => <div className={`smart-filter-row ${activeSavedFilterId === filter.id ? "selected" : ""}`} key={filter.id}><button className="nav-item smart-filter-item" title={filter.name} onClick={() => selectSavedFilter(filter)}><span className="nav-icon">◷</span><span>{filter.name}</span></button><button className="smart-filter-remove" aria-label={`删除智能集合 ${filter.name}`} title="删除智能集合" onClick={() => void removeSmartFilter(filter)}>×</button></div>)}

        <div className="sidebar-bottom">
          <div className="storage-label"><span>本地 Library</span><span>{roots.length} 个目录</span></div>
          <div className="storage-track"><span style={{ width: roots.length ? "42%" : "0%" }} /></div>
          <div className="storage-foot"><span>索引状态</span><span className="online"><i />{busy ? "处理中" : "就绪"}</span></div>
        </div>
      </aside>

      <main className="main-area">
        <header className="topbar">
          <div className="breadcrumbs"><span>Library</span><b>/</b><strong>{activeTitle}</strong></div>
          <form className="search-box" onSubmit={submitSearch}>
            <span>⌕</span><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索名称、文件名、路径或标签（空格分词）…" aria-label="搜索资产" />
            {query && <button type="button" className="clear-search" onClick={() => { setQuery(""); setSearchText(""); }}>×</button>}
            <kbd>ENTER</kbd>
          </form>
          <button className={`filter-button ${filterBuilderOpen ? "active" : ""}`} title="组合筛选并保存为智能集合" onClick={() => setFilterBuilderOpen((open) => !open)}><span>☷</span> 筛选 <i>⌄</i></button>
          <button className={`journal-button ${scanQueueOpen ? "active" : ""}`} onClick={() => setScanQueueOpen(true)}>扫描队列 {activeScans.length ? `(${activeScans.length})` : ""}</button>
          <button className="journal-button" onClick={() => void openOperationJournal()}>操作日志</button>
          <button className="avatar" aria-label="设置" title="设置" onClick={() => void openSettings()}>⚙</button>
        </header>

        <div className="library-content" ref={setLibraryScrollParent}>
          <div className="page-heading">
            <div><div className="eyebrow">YOUR COLLECTION</div><h1>{activeTitle}</h1><p>浏览、搜索并整理你的 MMD 资产</p></div>
            <div className="view-controls"><button className="open-model-button" onClick={() => void openModelPreview()}>＋ 打开 PMX</button><span className="asset-total"><b>{(showIndexedTotal ? indexedTotal : visibleAssets.length).toLocaleString()}{!showIndexedTotal && nextAssetCursor ? "+" : ""}</b> {showIndexedTotal ? "在库资产" : "已加载资产"}</span><label className="card-size-control">卡片大小 <input type="range" min="130" max="300" step="10" value={cardSize} aria-label="资产卡片大小" onChange={(event) => setCardSize(Number(event.target.value))} /></label></div>
          </div>

          <div className="type-tabs" role="tablist" aria-label="资产类型过滤">
            <button className={activeType === "all" ? "selected" : ""} onClick={() => selectCategory("all")}>全部 <span>{counts.all}</span></button>
            {(Object.keys(categoryLabels) as AssetType[]).map((type) => <button className={activeType === type ? "selected" : ""} key={type} onClick={() => selectCategory(type)}>{categoryLabels[type]} <span>{counts[type]}</span></button>)}
            <div className="tabs-spacer" />
            <button className="quick-add" disabled={busy} onClick={() => { setBulkSelectMode((mode) => !mode); setBulkSelectedIds(new Set()); }}>{bulkSelectMode ? "退出批量选择" : "批量选择"}</button>
            {activeType !== "all" && <button className="quick-add" onClick={() => void addRoot(activeType)}><span>＋</span> 添加目录</button>}
          </div>
          {activeType === "motion" && !activeSavedFilterId && !duplicatesOnly && <div className="motion-format-tabs" role="tablist" aria-label="动作文件格式">
            {([ ["all", "全部动作"], ["vmd", "VMD 动作"], ["vpd", "VPD 姿势"] ] as Array<[MotionFormat, string]>).map(([format, label]) =>
              <button key={format} role="tab" aria-selected={activeMotionFormat === format} className={activeMotionFormat === format ? "selected" : ""} onClick={() => selectMotionFormat(format)}>{label}</button>)}
          </div>}
          {folderRoot && !favoritesOnly && !duplicatesOnly && !activeSavedFilterId && <section className="folder-browser" aria-label="按文件夹浏览资产">
            <div className="folder-breadcrumbs"><button onClick={() => setActiveDirectory(null)}>{folderRoot.displayName}</button>{activeDirectory?.slice(folderRoot.path.length).split(/[\\/]/).filter(Boolean).map((part, index, all) => <button key={`${part}-${index}`} onClick={() => setActiveDirectory(`${folderRoot.path.replace(/[\\/]+$/, "")}\\${all.slice(0, index + 1).join("\\")}`)}>› {part}</button>)}</div>
            {childFolders.size > 0 && <div className="folder-children">{Array.from(childFolders.values()).sort((left, right) => left.path.localeCompare(right.path, "zh-CN")).map((folder) => <button key={folder.path} onClick={() => { setActiveDirectory(folder.path); setSelected(null); }} title={folder.path}><span>▤</span><strong>{folder.path.split(/[\\/]/).pop()}</strong><small>{folder.count.toLocaleString()}</small></button>)}</div>}
          </section>}

          {bulkSelectMode && <div className="bulk-actions" aria-label="批量操作">
            <span>已选择 {bulkSelectedIds.size} 项</span>
            <button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void setFavoriteForSelection(true)}>☆ 批量收藏</button>
            <button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void setFavoriteForSelection(false)}>取消收藏</button>
            <button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void addTagToSelection()}>＋ 批量添加标签</button>
            <button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void removeTagFromSelection()}>− 批量移除标签</button>
            <button disabled={busy || bulkSelectedIds.size === 0} onClick={() => void planMoveAssets(Array.from(bulkSelectedIds))}>移动资产包…</button>
            <button className="bulk-delete" disabled={busy || bulkSelectedIds.size === 0} onClick={() => void requestAssetOperation("recycle", Array.from(bulkSelectedIds))}>移到回收站…</button>
          </div>}

          {filterBuilderOpen && <section className="filter-builder"><div className="filter-builder-heading"><div><strong>组合筛选</strong><span>将条件保存为可复用的智能集合</span></div><button className="icon-button" aria-label="关闭筛选面板" onClick={() => setFilterBuilderOpen(false)}>×</button></div>
            <div className="filter-builder-name"><label htmlFor="smart-filter-name">集合名称</label><input id="smart-filter-name" value={filterName} onChange={(event) => setFilterName(event.target.value)} placeholder="例如：收藏的模型" /></div>
            <div className="filter-rule-list">{filterRules.map((rule, index) => <div className="filter-rule-row" key={index}>
              <select aria-label="筛选字段" value={rule.field} onChange={(event) => changeFilterField(index, event.target.value as FilterField)}>{(Object.keys(filterFieldLabels) as FilterField[]).map((field) => <option key={field} value={field}>{filterFieldLabels[field]}</option>)}</select>
              <select aria-label="比较方式" value={rule.operator} onChange={(event) => changeFilterRule(index, { operator: event.target.value as FilterOperator })}>{operatorsFor(rule.field).map((operator) => <option key={operator} value={operator}>{operatorLabels[operator]}</option>)}</select>
              {booleanFilterFields.has(rule.field) ? <select aria-label="布尔值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="true">是</option><option value="false">否</option></select>
                : rule.field === "assetType" ? <select aria-label="资产类型值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="model">模型</option><option value="motion">动作</option><option value="scene">场景</option></select>
                : rule.field === "rootId" && roots.length ? <select aria-label="资产根目录值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="">选择目录</option>{roots.map((root) => <option key={root.id} value={root.id}>{root.displayName}</option>)}</select>
                : rule.field === "cardStatus" ? <select aria-label="资源卡状态值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })}><option value="CardValid">有效</option><option value="CardMissing">缺失</option><option value="CardStale">过期</option><option value="CardBroken">损坏</option></select>
                : dateFilterFields.has(rule.field) ? <input aria-label="日期值" type="datetime-local" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })} />
                : numericFilterFields.has(rule.field) ? <input aria-label="数值" type="number" step="any" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })} />
                : <input aria-label="文本值" value={rule.value} onChange={(event) => changeFilterRule(index, { value: event.target.value })} placeholder="输入匹配内容" />}
              <label className="filter-not"><input type="checkbox" checked={rule.negate} onChange={(event) => changeFilterRule(index, { negate: event.target.checked })} />NOT</label>
              <button className="filter-rule-remove" aria-label="移除此条件" disabled={filterRules.length <= 1} onClick={() => setFilterRules((current) => current.filter((_, ruleIndex) => ruleIndex !== index))}>×</button>
            </div>)}</div>
            <div className="filter-builder-footer"><button className="filter-add-rule" onClick={() => setFilterRules((current) => [...current, { field: "tag", operator: "contains", value: "", negate: false }])}>＋ 添加条件</button><label className="filter-group-op">条件组合<select value={filterGroupOp} onChange={(event) => setFilterGroupOp(event.target.value as "and" | "or")}><option value="and">全部满足（AND）</option><option value="or">任一满足（OR）</option></select></label><span className="filter-builder-spacer" /><button className="filter-save" disabled={!filterName.trim()} onClick={() => void saveSmartFilter()}>保存并应用</button></div>
          </section>}

          {error && <div className="error-banner"><span>!</span><div><strong>操作未完成</strong><p>{error}</p></div><button onClick={() => setError("")}>×</button></div>}
          {notice && <div className="notice-banner"><span className={busy ? "spinner" : "notice-check"}>{busy ? "" : "✓"}</span><span>{notice}</span><button onClick={() => setNotice("")}>×</button></div>}

          {visibleAssets.length ? libraryScrollParent ? <VirtuosoGrid
            key={cardSize}
            data={visibleAssets}
            customScrollParent={libraryScrollParent}
            increaseViewportBy={{ top: 360, bottom: 720 }}
            endReached={() => void loadNextAssetPage()}
            computeItemKey={(_, asset) => asset.id}
            listClassName="asset-grid"
            style={{ "--card-min-width": `${cardSize}px` } as React.CSSProperties}
            itemClassName="asset-grid-item"
            itemContent={(index, asset) => <button className={`asset-card ${selected?.id === asset.id ? "selected" : ""} ${bulkSelectedIds.has(asset.id) ? "bulk-selected" : ""}`} aria-pressed={bulkSelectMode ? bulkSelectedIds.has(asset.id) : selected?.id === asset.id} onClick={() => toggleBulkSelection(asset)} onDoubleClick={() => { if (!bulkSelectMode) setPreviewAsset(asset); }} onContextMenu={(event) => openAssetMenu(event, asset)} style={{ animationDelay: `${Math.min(index, 18) * 18}ms` }}>
              <div className={`asset-art ${asset.assetType} ${asset.hasThumbnail ? "has-thumbnail" : ""}`}>
                {asset.hasThumbnail && <CardThumbnail assetId={asset.id} alt={`${asset.name} 缩略图`} />}
                <div className="art-orbit orbit-one" /><div className="art-orbit orbit-two" /><div className="art-glow" /><span className="art-glyph">{categoryGlyphs[asset.assetType]}</span><span className="art-format">{String(asset.metadata.file_type ?? asset.assetType).toUpperCase()}</span>{typeof asset.metadata.paired_camera_path === "string" && <span className="paired-camera-marker" title={`同目录配套镜头：${asset.metadata.paired_camera_path}`}>◉ 镜头</span>}{asset.isFavorite && <span className="favorite-marker">★</span>}{asset.cardStatus !== "CardValid" || !asset.hasThumbnail ? <span className="missing-preview">预览待生成</span> : null}
              </div>
              <div className="asset-card-body"><div className="asset-card-title" title={asset.name}>{asset.name}</div><div className="asset-card-subline"><span className={`badge ${asset.assetType}`}>{String(asset.metadata.is_camera_only ? "CAMERA" : asset.metadata.is_pose ? "POSE" : asset.assetType).toUpperCase()}</span><span className="asset-source-name" title={asset.primarySource}>{asset.primarySource.split(/[\\/]/).pop()}</span></div></div>
              {bulkSelectMode && <span className={`asset-bulk-checkbox ${bulkSelectedIds.has(asset.id) ? "checked" : ""}`} aria-hidden="true">{bulkSelectedIds.has(asset.id) ? "✓" : ""}</span>}
            </button>}
          /> : <div className="asset-grid-waiting" role="status">正在载入资产卡片…</div> : isRefreshing ? <div className="asset-grid-waiting" role="status">正在读取目录索引…</div> : <section className="empty-state">
            <div className="empty-illustration"><div className="empty-frame"><span className="empty-star">✳</span><span className="empty-orbit" /><span className="empty-base" /></div><div className="empty-spark spark-a">✦</div><div className="empty-spark spark-b">·</div></div>
            <div className="empty-kicker">A LIBRARY FOR YOUR MMD WORLD</div>
            <h2>{roots.length ? (searchText ? "没有找到匹配的资产" : "这个集合还没有资产") : "从你的第一个资产库开始"}</h2>
            <p>{roots.length ? (searchText ? "试试其他关键词，或清除搜索条件。" : "扫描已添加的目录，MMDbridgeLib 会为你的资产建立索引。") : "添加模型、动作或场景目录，建立一个可搜索、可整理的本地 MMD 资产库。"}</p>
            <div className="empty-actions">{(Object.keys(categoryLabels) as AssetType[]).map((type) => <button key={type} onClick={() => void addRoot(type)}><span>{categoryGlyphs[type]}</span>添加{categoryLabels[type]}目录</button>)}</div>
            <div className="privacy-note"><span>⌂</span>资产留在本机 · 移除目录不会删除文件</div>
          </section>}
          {(nextAssetCursor || loadingNextPage) && <div className="asset-grid-footer" role="status">{loadingNextPage ? "正在载入更多资产…" : `已载入 ${visibleAssets.length.toLocaleString()} 项，继续向下滚动以载入下一页`}</div>}
        </div>

      <footer className={`jobbar ${jobsExpanded ? "expanded" : ""}`}>
        <div className="jobbar-leading"><span className={`jobbar-indicator ${busy || activeThumbnailCount || activeScans.length ? "working" : ""}`} />{busy ? "正在处理资产" : "后台任务"}<span className="jobbar-count">{activeThumbnailCount + activeScans.length + (busy ? 1 : 0)}</span></div>
        <div className="jobbar-detail">
          {busy ? notice : activeScans.length ? `${activeScans.length} 个扫描任务 · ${scanStatusLabels[activeScans[0].status]} ${Math.round(activeScans[0].progress * 100)}%` : activeThumbnailCount ? `${activeThumbnailCount} 个缩略图任务 · 已完成 ${jobSummary.Completed ?? 0} · 失败 ${jobSummary.Failed ?? 0}` : "没有进行中的任务"}
        </div>
        {scanStates.length > 0 && <button className="jobbar-scan-link" onClick={() => setScanQueueOpen(true)}>管理扫描</button>}
        {totalThumbnailCount > 0 && <button className="jobbar-chevron" aria-label={jobsExpanded ? "收起缩略图任务" : "展开缩略图任务"} onClick={() => setJobsExpanded((expanded) => !expanded)}>{jobsExpanded ? "⌄" : "⌃"}</button>}
        {jobsExpanded && <div className="jobs-panel" aria-label="后台任务列表">
          <div className="jobs-panel-heading"><strong>缩略图任务</strong><span>{totalThumbnailCount} 项 · 显示最近 12 项</span></div>
          {jobs.length ? jobs.slice(0, 12).map((job) => <div className="jobs-panel-row" key={job.id}>
            <div className="jobs-panel-main"><strong>缩略图</strong><span>{job.status}{["Pending", "Parsing", "Rendering", "Encoding"].includes(job.status) ? ` · ${Math.round(job.progress * 100)}%` : job.error?.message ? ` · ${job.error.message}` : ""}</span></div>
            {activeJobs.some((activeJob) => activeJob.id === job.id) && <button onClick={() => void cancelJob(job.id)}>取消</button>}
            {["Failed", "Cancelled"].includes(job.status) && <button onClick={() => void retryJob(job.id)}>重试</button>}
          </div>) : <div className="jobs-panel-empty">没有缩略图任务</div>}
        </div>}
      </footer>
      </main>

      <aside className={`inspector ${selected ? "has-selection" : ""}`}>
        <div className="inspector-top"><div><div className="eyebrow">ASSET INSPECTOR</div><h2>资产详情</h2></div><button className="icon-button" aria-label="关闭详情" onClick={() => setSelected(null)}>×</button></div>
        {selected ? <>
          <button className={`inspector-preview ${selected.assetType} ${selected.hasThumbnail ? "has-thumbnail" : ""}`} title="查看缩略图" onClick={() => setPreviewAsset(selected)}>{selected.hasThumbnail && <CardThumbnail assetId={selected.id} alt={`${selected.name} 缩略图`} />}<div className="art-orbit orbit-one" /><div className="art-orbit orbit-two" /><span className="inspector-glyph">{categoryGlyphs[selected.assetType]}</span><span className="preview-badge">{String(selected.metadata.is_camera_only ? "CAMERA" : selected.metadata.is_pose ? "POSE" : selected.assetType).toUpperCase()}</span></button>
          <div className="inspector-title"><div><div className="inspector-type">{categoryLabels[selected.assetType]}</div><h3>{selected.name}</h3></div><button className={`favorite-button ${selected.isFavorite ? "favorited" : ""}`} title={selected.isFavorite ? "取消收藏" : "添加收藏"} onClick={() => void toggleFavorite(selected)}>{selected.isFavorite ? "★" : "☆"}</button></div>
          <div className="inspector-section"><div className="section-title">基本信息</div><div className="detail-list">
            {Object.entries(selected.metadata).filter(([key, value]) => metadataLabels[key] && (typeof value === "number" || typeof value === "boolean" || typeof value === "string")).slice(0, 10).map(([key, value]) => <div className="detail-row" key={key}><span>{metadataLabels[key]}</span><strong>{formatValue(value)}</strong></div>)}
            <div className="detail-row"><span>资产状态</span><strong className={selected.statuses.includes("ParseFailed") || selected.statuses.includes("MissingSource") ? "state-warn" : "state-ready"}>{assetStatusText(selected.statuses)}</strong></div>
            {selected.statuses.includes("NeedsReview") && reviewReason(selected) && <div className="detail-row review-reason"><span>资源提示</span><strong>{reviewReason(selected)}</strong></div>}
            <div className="detail-row"><span>资源卡</span><strong className={selected.cardStatus === "CardValid" ? "state-ready" : "state-muted"}>{selected.cardStatus}{selected.cardStatus === "CardValid" && !selected.hasThumbnail ? " · 预览待生成" : ""}</strong><span>{(selected.cardStatus !== "CardValid" || !selected.hasThumbnail) && <button className="tiny-link" disabled={busy} onClick={() => void createCard(selected.id)}>创建 / 刷新</button>}<button className="tiny-link" disabled={busy} onClick={() => void verifyCard(selected.id)}>校验</button></span></div>
          </div></div>
          <div className="inspector-section source-section"><div className="section-title">源文件</div><div className="source-path" title={selected.primarySource}><span className="file-icon">▧</span><div><strong>{selected.primarySource.split(/[\\/]/).pop()}</strong><small>{selected.assetDirectory}</small></div></div><div className="asset-file-actions"><button disabled={busy} onClick={() => void planRenameAsset(selected)}>重命名资产包</button><button disabled={busy} onClick={() => void planMoveAssets([selected.id])}>移动…</button><button disabled={busy} onClick={() => void requestAssetOperation("recycle", [selected.id])}>移到回收站…</button></div></div>
          <div className="inspector-section tags-section"><div className="section-title">标签 <button className="add-tag" title="添加用户标签" onClick={() => void addTag()}>＋</button></div>{assetTags.length ? <div className="tag-list">{assetTags.map((tag) => <span className={`tag-chip ${tag.source}`} key={`${tag.name}-${tag.source}`} title={`来源：${tag.source}`}>{tag.name}<button aria-label={`移除标签 ${tag.name}`} onClick={() => void removeTag(tag.name)}>×</button></span>)}</div> : <div className="tag-empty">尚未添加标签</div>}</div>
          {assetRelations.length > 0 && <div className="inspector-section relation-section"><div className="section-title">关系与版本 <span className="relation-count">{assetRelations.length}</span></div><div className="relation-list">{assetRelations.map((relation) => {
            const otherPath = selected.id === relation.sourceAsset ? relation.reason.target_path : relation.reason.source_path;
            const otherName = typeof otherPath === "string" ? otherPath.split(/[\\/]/).pop() : (selected.id === relation.sourceAsset ? relation.targetAsset : relation.sourceAsset).slice(0, 8);
            const reasonCodes = Array.isArray(relation.reason.reason_codes) ? relation.reason.reason_codes.filter((reason): reason is string => typeof reason === "string") : [];
            const relationName = relation.relationType === "MotionCameraPair" ? "动作 / Camera" : "版本族";
            return <div className="relation-card" key={relation.id} title={reasonCodes.join(" · ")}><div className="relation-card-main"><strong>{relationName}</strong><span>{otherName}</span></div><div className="relation-card-meta"><span>{Math.round(relation.confidence * 100)}% · {relation.confirmed ? "已确认" : "待确认"}</span>{!relation.confirmed && <button className="tiny-link" onClick={() => void confirmRelation(relation)}>确认</button>}</div></div>;
          })}</div></div>}
          {assetDuplicates.length > 0 && <div className="inspector-section duplicate-section"><div className="section-title">重复项建议 <span className="relation-count">{assetDuplicates.length}</span></div><div className="relation-list">{assetDuplicates.map((duplicate) => {
            const isA = selected.id === duplicate.assetA;
            const otherName = isA ? duplicate.assetBName : duplicate.assetAName;
            const otherPath = isA ? duplicate.assetBPath : duplicate.assetAPath;
            const reasonCodes = Array.isArray(duplicate.reason.reason_codes) ? duplicate.reason.reason_codes : [];
            const exact = reasonCodes.includes("exact_content_hash");
            return <div className="relation-card" key={duplicate.id} title={otherPath}><div className="relation-card-main"><strong>{exact ? "内容完全相同" : "可能重复"}</strong><span>{otherName}</span></div><div className="relation-card-meta"><span>{exact ? "BLAKE3 内容相同" : "名称、文件与结构相似"}</span><span>{Math.round(duplicate.similarity * 100)}%</span></div></div>;
          })}</div></div>}
          <div className="inspector-spacer" />
          <div className="inspector-actions"><button onClick={() => void revealAsset(selected)} title="在资源管理器中定位源文件"><span>↗</span> 在文件夹中显示</button><button onClick={() => void openAssetDirectory(selected)} title="打开资产源目录">打开目录</button>{selected.assetType !== "motion" && <button className="preview-3d-button" onClick={() => setViewerAsset({ id: selected.id, name: selected.name, primarySource: selected.primarySource, assetType: selected.assetType as "model" | "scene" })}><span>◇</span> 3D 预览</button>}{selected.assetType === "motion" && selected.primarySource.toLowerCase().endsWith(".vmd") && <button className="preview-3d-button" onClick={() => setMotionViewerAsset(selected)}><span>▶</span> 3D 动作</button>}</div>
        </> : <div className="inspector-empty"><div className="inspector-empty-icon">◇</div><strong>选择一个资产</strong><span>详细信息将在此处显示</span></div>}
      </aside>
      {assetMenu && <div className="asset-context-menu" ref={assetMenuRef} role="menu" aria-label={`${assetMenu.asset.name} 操作`} style={{ left: assetMenu.x, top: assetMenu.y }}>
        <button role="menuitem" onClick={() => { setPreviewAsset(assetMenu.asset); setAssetMenu(null); }}>查看缩略图</button>
        {assetMenu.asset.assetType !== "motion" && <button role="menuitem" onClick={() => { setViewerAsset({ id: assetMenu.asset.id, name: assetMenu.asset.name, primarySource: assetMenu.asset.primarySource, assetType: assetMenu.asset.assetType as "model" | "scene" }); setAssetMenu(null); }}>查看 3D {assetMenu.asset.assetType === "scene" ? "场景" : "模型"}</button>}
        {assetMenu.asset.assetType === "motion" && assetMenu.asset.primarySource.toLowerCase().endsWith(".vmd") && <button role="menuitem" onClick={() => { setMotionViewerAsset(assetMenu.asset); setAssetMenu(null); }}>播放 3D 动作{typeof assetMenu.asset.metadata.paired_camera_path === "string" ? " / 配套镜头" : ""}</button>}
        <div className="asset-context-separator" role="separator" />
        <button role="menuitem" onClick={() => { void revealAsset(assetMenu.asset); setAssetMenu(null); }}>在文件夹中显示</button>
        <button role="menuitem" onClick={() => { void openAssetDirectory(assetMenu.asset); setAssetMenu(null); }}>打开所在目录</button>
        <div className="asset-context-separator" role="separator" />
        <button role="menuitem" onClick={() => { void toggleFavorite(assetMenu.asset); setAssetMenu(null); }}>{assetMenu.asset.isFavorite ? "取消收藏" : "添加收藏"}</button>
        <button role="menuitem" disabled={busy} onClick={() => { void createCard(assetMenu.asset.id); setAssetMenu(null); }}>刷新资源卡与缩略图</button>
      </div>}
      {scanQueueOpen && <div className="operation-modal-backdrop" role="presentation"><section className="operation-modal scan-queue-modal" role="dialog" aria-modal="true" aria-labelledby="scan-queue-title">
        <div className="settings-modal-heading"><div><span>SCAN QUEUE</span><h2 id="scan-queue-title">扫描索引队列</h2></div><button className="icon-button" aria-label="关闭扫描队列" onClick={() => setScanQueueOpen(false)}>×</button></div>
        <p className="operation-summary">扫描按队列顺序依次执行。暂停会保留已建立的索引；继续时会重新核对该目录。</p>
        <div className="scan-queue-list">{scanStates.length ? scanStates.map((scan) => {
          const root = roots.find((item) => item.id === scan.rootId);
          const pending = scanStates.filter((item) => item.status === "Pending").sort((a, b) => a.queueOrder - b.queueOrder);
          const pendingIndex = pending.findIndex((item) => item.rootId === scan.rootId);
          return <div className="scan-queue-row" key={scan.rootId}>
            <div className="scan-queue-main"><strong>{root?.displayName ?? scan.rootId}{scan.fullCheck ? " · 完整检查" : ""}</strong><span>{scanStatusLabels[scan.status] ?? scan.status} · {scan.filesProcessed.toLocaleString()}/{scan.filesSeen.toLocaleString()} 文件 · {Math.round(scan.progress * 100)}%{scan.error ? ` · ${scan.error}` : ""}</span><div className="scan-progress"><span style={{ width: `${Math.round(scan.progress * 100)}%` }} /></div></div>
            <div className="scan-queue-actions">
              {scan.status === "Pending" && <><button disabled={pendingIndex <= 0} aria-label={`${root?.displayName ?? "扫描"}上移`} onClick={() => void moveScan(scan.rootId, -1)}>↑</button><button disabled={pendingIndex >= pending.length - 1} aria-label={`${root?.displayName ?? "扫描"}下移`} onClick={() => void moveScan(scan.rootId, 1)}>↓</button></>}
              {["Pending", "Discovering", "Indexing", "Verifying", "Relations", "Duplicates"].includes(scan.status) && <button onClick={() => void pauseScan(scan.rootId)}>暂停</button>}
              {scan.status === "Paused" && root?.enabled && <button onClick={() => void scanRoot(root)}>继续</button>}
              {activeScanStatuses.has(scan.status) || scan.status === "Paused" ? <button onClick={() => void cancelScan(scan.rootId)}>停止</button> : null}
              {["Failed", "Cancelled", "Completed"].includes(scan.status) && root?.enabled && <button onClick={() => void scanRoot(root)}>{scan.status === "Completed" ? "重扫" : "重试"}</button>}
              {["Failed", "Cancelled", "Completed"].includes(scan.status) && root?.enabled && <button onClick={() => void scanRoot(root, true)}>完整检查</button>}
            </div>
          </div>;
        }) : <div className="jobs-panel-empty">队列为空。点击目录旁的扫描按钮加入任务。</div>}</div>
      </section></div>}
      {previewAsset && <div className="operation-modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setPreviewAsset(null); }}><section className="operation-modal thumbnail-preview-modal" role="dialog" aria-modal="true" aria-labelledby="thumbnail-preview-title">
        <div className="settings-modal-heading"><div><span>ASSET THUMBNAIL</span><h2 id="thumbnail-preview-title">{previewAsset.name}</h2></div><button className="icon-button" aria-label="关闭缩略图" onClick={() => setPreviewAsset(null)}>×</button></div>
        <div className="thumbnail-preview-frame">{previewAsset.hasThumbnail ? <CardThumbnail assetId={previewAsset.id} alt={`${previewAsset.name} 缩略图`} /> : <div className="thumbnail-preview-empty">该资产尚未生成缩略图</div>}</div>
        {!previewAsset.hasThumbnail && <div className="settings-modal-actions"><button onClick={() => { void createCard(previewAsset.id); setPreviewAsset(null); }}>生成资源卡和缩略图</button></div>}
      </section></div>}
      {viewerAsset && <Suspense fallback={<div role="status" style={{ position: "fixed", inset: 0, zIndex: 50, display: "grid", placeItems: "center", background: "#050a0de8", color: "#bbcbc9", fontSize: 12 }}>正在载入 3D Viewer…</div>}><ModelViewer asset={viewerAsset} onClose={() => setViewerAsset(null)} /></Suspense>}
      {motionViewerAsset && <Suspense fallback={<div role="status" style={{ position: "fixed", inset: 0, zIndex: 50, display: "grid", placeItems: "center", background: "#050a0de8", color: "#bbcbc9", fontSize: 12 }}>正在载入 VMD 3D Viewer…</div>}><MotionViewer asset={motionViewerAsset} onClose={() => setMotionViewerAsset(null)} /></Suspense>}
      {assetOperationPlan && <div className="operation-modal-backdrop" role="presentation"><section className="operation-modal" role="dialog" aria-modal="true" aria-labelledby="operation-plan-title">
        <div className="settings-modal-heading"><div><span>PACKAGE OPERATION</span><h2 id="operation-plan-title">确认资产操作</h2></div><button className="icon-button" aria-label="关闭操作计划" onClick={() => setAssetOperationPlan(null)}>×</button></div>
        <p className="operation-summary">{assetOperationPlan.operation === "move" ? "移动" : assetOperationPlan.operation === "rename" ? "重命名" : "发送到 Windows 回收站"}将影响 {assetOperationPlan.sourcePaths.length} 个资产包、{assetOperationPlan.affectedAssets.length} 项索引资产。</p>
        <div className="operation-path-list">{assetOperationPlan.sourcePaths.map((source, index) => <div className="operation-path-row" key={source}><span>{source}</span>{assetOperationPlan.destinationPaths[index] && <><b>→</b><span>{assetOperationPlan.destinationPaths[index]}</span></>}</div>)}</div>
        {assetOperationPlan.affectedAssets.length > 0 && <div className="operation-assets"><strong>受影响资产</strong><div>{assetOperationPlan.affectedAssets.slice(0, 12).map((asset) => <span key={asset.id}>{asset.name}</span>)}{assetOperationPlan.affectedAssets.length > 12 && <span>还有 {assetOperationPlan.affectedAssets.length - 12} 项</span>}</div></div>}
        {assetOperationPlan.dependencyPaths.length > 0 && <div className="operation-assets"><strong>包内依赖文件 · {assetOperationPlan.dependencyPaths.length}</strong><div>{assetOperationPlan.dependencyPaths.slice(0, 8).map((path) => <span title={path} key={path}>{path}</span>)}{assetOperationPlan.dependencyPaths.length > 8 && <span>还有 {assetOperationPlan.dependencyPaths.length - 8} 个依赖</span>}</div></div>}
        {assetOperationPlan.warnings.length > 0 && <div className="operation-warnings"><strong>安全检查未通过</strong>{assetOperationPlan.warnings.map((warning) => <p key={warning}>{warning}</p>)}</div>}
        <footer className="operation-modal-actions"><button onClick={() => setAssetOperationPlan(null)}>取消</button><button className={assetOperationPlan.operation === "recycle" ? "danger" : "primary"} disabled={!assetOperationPlan.canExecute || busy} onClick={() => void executePlannedAssetOperation()}>{busy ? "正在执行…" : assetOperationPlan.operation === "recycle" ? "确认移到回收站" : "确认执行"}</button></footer>
      </section></div>}
      {operationJournalOpen && <div className="operation-modal-backdrop" role="presentation"><section className="operation-modal journal-modal" role="dialog" aria-modal="true" aria-labelledby="operation-journal-title">
        <div className="settings-modal-heading"><div><span>FILE OPERATION HISTORY</span><h2 id="operation-journal-title">资产操作日志</h2></div><button className="icon-button" aria-label="关闭操作日志" onClick={() => setOperationJournalOpen(false)}>×</button></div>
        <div className="journal-heading-row"><span>记录保留在本地数据库；“需要恢复”表示文件操作部分完成或索引更新失败。</span><button disabled={busy} onClick={() => void openOperationJournal()}>刷新</button></div>
        <div className="journal-entry-list">{operationJournal.length ? operationJournal.map((entry) => <article className="journal-entry" key={entry.id}>
          <div className="journal-entry-heading"><strong>{entry.operation === "move" ? "移动资产包" : entry.operation === "rename" ? "重命名资产包" : "移到回收站"}</strong><span className={`journal-status ${entry.status === "RecoveryNeeded" || entry.status === "Started" ? "needs-recovery" : entry.status.toLowerCase()}`}>{entry.status === "RecoveryNeeded" || entry.status === "Started" ? "需要检查" : entry.status === "Completed" ? "完成" : entry.status === "Resolved" ? "已核对" : "失败"}</span></div>
          <div className="journal-entry-paths">{entry.sourcePaths.map((source, index) => <div key={`${entry.id}-${source}`}><span>{source}</span>{entry.destinationPaths[index] && <><b>→</b><span>{entry.destinationPaths[index]}</span></>}</div>)}</div>
          <div className="journal-entry-result">{entry.affectedAssetCount} 项资产 · {entry.result?.message ?? "没有结果说明"} · {new Date(entry.updatedAt).toLocaleString()}</div>
          {entry.status === "RecoveryNeeded" && <button className="journal-resolve-button" onClick={() => void resolveJournalEntry(entry)}>已人工恢复并重扫，标记已核对</button>}
        </article>) : <div className="jobs-panel-empty">暂无资产文件操作记录</div>}</div>
      </section></div>}
      {settingsOpen && <div className="settings-modal-backdrop" role="presentation"><section className="settings-modal" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <div className="settings-modal-heading"><div><span>LIBRARY PREFERENCES</span><h2 id="settings-title">设置</h2></div><button className="icon-button" aria-label="关闭设置" onClick={() => setSettingsOpen(false)}>×</button></div>
        <div className="settings-field"><label>Motion Preview Model</label><p>用于 VMD/VPD 缩略图的 PMX 模型。设置后，VMD 会预览首帧或第一关键帧并应用骨骼、顶点/组、UV、材质、Flip 与 Impulse 表情（Impulse 为静帧近似）；文件含镜头轨道时也会按该帧相机取景，没有镜头轨道时按模型自动取景。VPD 会应用姿势骨骼。</p><div className="settings-model-path" title={motionPreviewModel ?? "尚未设置"}>{motionPreviewModel ?? "尚未设置模型"}</div><div className="settings-modal-actions"><button disabled={settingsBusy} onClick={() => void chooseMotionPreviewModel()}>{settingsBusy ? "正在保存…" : "选择 PMX 模型"}</button><button disabled={settingsBusy || !motionPreviewModel} onClick={() => void clearMotionPreviewModel()}>清除</button></div></div>
        <div className="settings-field concurrency-settings"><label>缩略图阶段并发上限</label><p>分别限制解析、GPU 渲染和 WebP 编码。自动模式会按设备资源选择；每阶段可设 1–8 路，渲染自动模式为 1 路。</p>
          {(["parse", "render", "encode"] as const).map((stage) => {
            const value = thumbnailConcurrencyDraft[stage];
            const selection = value === null ? "auto" : ([1, 2, 4, 8].includes(value) ? String(value) : "custom");
            const label = stage === "parse" ? "解析 Parse" : stage === "render" ? "渲染 Render" : "编码 Encode";
            return <div className="concurrency-row" key={stage}><span>{label}</span><div className="concurrency-controls"><select aria-label={`${label}并发上限`} value={selection} disabled={settingsBusy} onChange={(event) => {
              const selected = event.target.value;
              setThumbnailConcurrencyDraft((current) => ({ ...current, [stage]: selected === "auto" ? null : selected === "custom" ? (current[stage] ?? 3) : Number(selected) }));
            }}><option value="auto">Auto</option><option value="1">1</option><option value="2">2</option><option value="4">4</option><option value="8">8</option><option value="custom">自定义</option></select>
              {selection === "custom" && <input aria-label={`${label}自定义并发数`} type="number" min={1} max={8} step={1} value={value ?? 3} disabled={settingsBusy} onChange={(event) => {
                const number = Number(event.target.value);
                if (Number.isInteger(number) && number >= 1 && number <= 8) setThumbnailConcurrencyDraft((current) => ({ ...current, [stage]: number }));
              }} />}</div></div>;
          })}
          <div className="settings-modal-actions"><button disabled={settingsBusy} onClick={() => void saveThumbnailConcurrency()}>{settingsBusy ? "正在保存…" : "保存并发设置"}</button></div>
        </div>
        {storageInfo && <div className="settings-field storage-settings"><label>便携数据库</label><p title={storageInfo.path}>{storageInfo.path}</p><div>数据库 {formatMiB(storageInfo.databaseBytes)} / {formatMiB(storageInfo.databaseLimitBytes)} · WAL {formatMiB(storageInfo.walBytes)}（目标不超过 {formatMiB(storageInfo.walTargetBytes)}）</div><div className="settings-modal-actions"><button disabled={settingsBusy || activeScans.length > 0 || activeThumbnailCount > 0} onClick={() => void compactStorage()}>整理数据库和历史任务</button></div></div>}
        <footer>模型文件保留在原位置；数据库保存在程序旁的 data 目录。</footer>
      </section></div>}
    </div>
  );
}
