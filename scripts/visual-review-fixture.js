/* Isolated review fixture; never included in the desktop application build. */
const params = new URLSearchParams(location.search);
const requestedScheme = params.get('scheme');
if (requestedScheme) localStorage.setItem('mmdbridge-color-scheme', requestedScheme);
const names = ['初音ミク · 制服', '夜に駆ける', '白いアトリエ', '洛天依 · 冬装', 'Hand in Hand', '雨上がりの街', '雪ミク · 2026', 'アイドル', '小さな図書館', '乐正绫 · 生日', 'Ballet · Pose', '夕暮れのステージ', '巡音ルカ · Classic', 'シネマ', '海辺の駅', '镜音リン · Casual', '踊', '星空のテラス'];
const kinds = ['model', 'motion', 'scene'];
const roots = kinds.map((assetType, i) => ({ id: 'root-' + i, assetType, path: 'E:\\MMD\\' + ['Models', 'Motions', 'Stages'][i], displayName: ['角色模型', '舞蹈与姿势', '舞台场景'][i], enabled: true, scanRecursive: true, scanStatus: 'Completed' }));
const assets = names.map((name, i) => {
 const assetType = kinds[i % 3], root = roots[i % 3];
 const metadata = assetType === 'motion' ? {file_type: 'VMD', total_frames: 4320 + i * 80, duration_seconds: 144 + i * 2, has_bone_motion: true, has_morph_motion: true} : {file_type: 'PMX', polygon_count: 28400 + i * 1580, bone_count: 172, skeleton_class: 'standard'};
 return {id: 'asset-' + i, assetType, rootId: root.id, name, primarySource: root.path + '\\' + name + (assetType === 'motion' ? '.vmd' : '.pmx'), assetDirectory: root.path + '\\' + name, metadata, statuses: ['Ready'], cardStatus: 'CardValid', hasThumbnail: false, isFavorite: [0,3,7].includes(i), updatedAt: '2026-10-05T03:50:00Z'};
});
const reviewJournal = [{
 id: 'review-recovery', operation: 'move', status: params.get('review') === 'live' ? 'Started' : 'RecoveryNeeded',
 sourcePaths: ['E:\\MMD\\Models\\初音ミク'], destinationPaths: ['E:\\MMD\\Archive\\初音ミク'],
 affectedAssetCount: 3, createdAt: '2026-10-05T14:10:00Z', updatedAt: '2026-10-05T14:10:00Z',
 result: {message: params.get('review') === 'live' ? '正在移动资产包' : '部分路径完成移动，其他路径需要检查后恢复',
 completedPaths: ['E:\\MMD\\Models\\初音ミク'], uncertainPaths: ['E:\\MMD\\Models\\洛天依'],
 notStartedPaths: ['E:\\MMD\\Models\\乐正绫'], indexUpdateFailed: true}
}];
if (params.get('review') === 'live') {
 delete reviewJournal[0].result.uncertainPaths;
 delete reviewJournal[0].result.indexUpdateFailed;
 reviewJournal[0].result.notStartedPaths.push('E:\\MMD\\Models\\洛天依');
}
const empty = params.get('state') === 'empty';
let callback = 0;
window.__TAURI_INTERNALS__ = {
 transformCallback: () => ++callback, unregisterCallback: () => {},
 invoke: async (cmd, args = {}) => {
  if (cmd === 'operation_journal_list') return reviewJournal;
  if (cmd === 'operation_journal_resolve') {
   const entry = reviewJournal.find(e => e.id === args.operationId);
   if (!entry || entry.status !== 'RecoveryNeeded') return false;
   entry.status = 'Resolved'; entry.updatedAt = '2026-10-05T14:12:00Z';
   entry.result = {...entry.result, message: '用户已确认手动恢复并重新扫描', resolvedAt: entry.updatedAt};
   return true;
  }
  if (cmd === 'startup_status') return {ready: true, phase: '就绪', step: 6, detail: '', completed: 1, total: 1, elapsed_ms: 30, phase_elapsed_ms: 10, idle_ms: 0, error: null};
  if (cmd === 'roots_list') return empty ? [] : roots;
  if (cmd === 'asset_counts') return empty ? {all: 0, model: 0, motion: 0, scene: 0, byRoot: {}} : {all: 18, model: 6, motion: 6, scene: 6, byRoot: {'root-0': 6, 'root-1': 6, 'root-2': 6}};
  if (cmd === 'assets_page') { let items = empty ? [] : assets.filter(a => (!args.assetType || a.assetType === args.assetType) && (!args.rootId || a.rootId === args.rootId) && (!args.favoritesOnly || a.isFavorite) && (!args.query || a.name.toLowerCase().includes(args.query.toLowerCase()))); return {items, nextCursor: null}; }
  if (cmd === 'asset_inspect') return assets.find(a => a.id === args.assetId);
  if (cmd === 'asset_tags') return [{name: 'MMD', source: 'parser', confidence: 1}, {name: '舞台演出', source: 'user', confidence: null}];
  if (cmd === 'tags_list') return ['MMD', '舞台演出', '初音ミク', 'Vsinger'];
  if (cmd === 'relations_list' || cmd === 'jobs_list' || cmd === 'scan_states' || cmd === 'filters_list') return [];
  if (cmd === 'jobs_summary') return {};
  if (cmd === 'motion_preview_model_get') return null;
  if (cmd === 'thumbnail_concurrency_get') return {parse: null, render: null, encode: null};
  if (cmd === 'storage_info') return {path: 'E:\\MMD\\MBL\\data\\library.sqlite3', databaseBytes: 16000000, walBytes: 0, databaseLimitBytes: 1000000000, walTargetBytes: 64000000};
  if (cmd === 'asset_directories') return [{path: roots.find(r => r.id === args.rootId)?.path + '\\角色', count: 6}];
  if (cmd === 'asset_directory_page') return {path: args.path || roots.find(r => r.id === args.rootId)?.path, visibleCount: 6, childDirectories: [], adjusted: false};
  if (cmd === 'favorite_set') { const a = assets.find(a => a.id === args.assetId); a.isFavorite = args.favorite; return true; }
  if (cmd === 'card_verify') return {status: 'CardValid'};
  if (cmd.startsWith('plugin:event|')) return callback;
  if (cmd === 'library_background_start') return null;
  throw new Error('Review fixture does not support ' + cmd);
 }
};
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {unregisterListener: () => {}};
