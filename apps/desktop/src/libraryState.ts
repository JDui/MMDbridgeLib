import { readPreference } from "./preferences";

export type AssetViewState = {
  activeType: "model" | "motion" | "scene" | "all";
  activeMotionFormat: "all" | "vmd" | "vpd";
  activeRoot: string | null;
  activeDirectory: string | null;
  activeSavedFilterId: string | null;
  favoritesOnly: boolean;
  query: string;
  searchText: string;
  scrollTop: number;
  selectedTags: string[];
  tagMatch: "and" | "or";
  skeletonClass: "all" | "standard" | "nonstandard" | "unknown";
  recursiveScope: boolean;
};

export function readAssetViewState(key: string): AssetViewState | null {
  try {
    const stored = readPreference(key);
    if (!stored) return null;
    const value = JSON.parse(stored) as Partial<AssetViewState> | null;
    if (!value || !["all", "model", "motion", "scene"].includes(value.activeType ?? "")
      || !["all", "vmd", "vpd"].includes(value.activeMotionFormat ?? "")
      || ![value.activeRoot, value.activeDirectory, value.activeSavedFilterId].every((item) => item === null || typeof item === "string")
      || typeof value.favoritesOnly !== "boolean" || typeof value.query !== "string" || typeof value.searchText !== "string"
      || typeof value.scrollTop !== "number" || !Number.isFinite(value.scrollTop) || value.scrollTop < 0
      || (value.selectedTags !== undefined && (!Array.isArray(value.selectedTags) || value.selectedTags.length > 24 || !value.selectedTags.every((tag) => typeof tag === "string" && tag.length > 0)))
      || (value.tagMatch !== undefined && !["and", "or"].includes(value.tagMatch))
      || (value.skeletonClass !== undefined && !["all", "standard", "nonstandard", "unknown"].includes(value.skeletonClass))
      || (value.recursiveScope !== undefined && typeof value.recursiveScope !== "boolean")) return null;
    return { ...value, selectedTags: value.selectedTags ?? [], tagMatch: value.tagMatch ?? "and", skeletonClass: value.skeletonClass ?? "all", recursiveScope: value.recursiveScope ?? true } as AssetViewState;
  } catch { return null; }
}

export function visibleSelection(ids: Set<string>, items: Array<{ id: string }>): Set<string> {
  const visible = new Set(items.map((item) => item.id));
  return new Set([...ids].filter((id) => visible.has(id)));
}

export const activeThumbnailStatuses = new Set(["Pending", "Parsing", "Rendering", "Encoding", "Cancelling"]);
