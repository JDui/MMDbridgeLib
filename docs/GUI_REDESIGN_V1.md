# MMDbridgeLib GUI / UX 全局审查与优化 V1

日期：2026-10-01。产品：Windows 本地 MMD Asset Manager。依据：用户附带的 Phase 0–10 任务、`AGENTS.md`、`MMDbridgeLib_Goal_v1.md`、当前 `docs/`、桌面源码及最近 Mantine / layout / Folder Browser 提交。

## 执行范围与产品结论

当前产品已经具备 Library 管理的主要能力，应继续围绕“找到资产 → 判断是否可用 → 预览 → 收藏/整理 → 安全操作 → 看懂后台进度”优化。保留 Steam Library 式三栏、Thumbnail 主视觉、React/TypeScript/Mantine/Tauri v2、Virtuoso 和 Core 业务边界。

本次先完成 Phase 0 基线，随后只实施可独立审查的局部修复。整页组件拆分、查询状态重构、Inspector 大幅重排属于新的较大 GUI 改造，需要用户确认本文件的具体范围后推进；不能把下方设计规格当作全部已经完成。

旧规格里的重复项、X 和 NeedsReview 要服从后续用户要求，不恢复这些能力；纯 Camera 保留为动作辅助数据，不加入普通 Library 列表。

证据分为：**源码确认**、**本轮离屏夹具**、**待实机验证**。旧审查截图和历史记录只用于复用方法，不计为本次通过。基线使用本轮构建，并冻结在 `target/gui-redesign-audit/baseline-dist/`；截图/模拟 IPC 不连接真实数据库，不操作真实资产，不启动或抢占前台窗口。

## 1. Current UI Architecture

| 层 | 当前实现 | 保留边界 |
| --- | --- | --- |
| 启动 | `main.tsx`：Core startup_status 轮询、阶段/数量/耗时、错误页、延迟加载 App | 不改初始化和缩略图版本检查语义 |
| Shell / 业务状态 | `App.tsx`：导航、查询、Root、扫描、任务、选择、筛选、文件操作、设置和弹窗 | Core 数据不在前端重新推断 |
| Library | AppShell、Mantine Tabs/MultiSelect、VirtuosoGrid、120 条 cursor page | 保留 customScrollParent、stable asset ID、分页和懒加载 |
| Thumbnail | CardThumbnail、四路读取上限、AbortController、Blob URL 清理、revision 重载 | 不改 Core 渲染、缓存和 manifest |
| 通用浮层 | `LibraryDialog.tsx`、`LibraryContextMenu.tsx`、Mantine Modal / Menu | 复用取消语义、focus trap、Escape 和键盘导航 |
| Viewer | ModelViewer / MotionViewer 分别 lazy import，共用 viewerRendering，独立 CSS | 不改几何、权重、变形、相机/时间算法 |
| 视觉 | theme.ts、styles.css、mantine-layout.css、virtualized-grid.css 和 Viewer CSS | Mantine 管控件，产品 CSS 管布局与资产展示 |
| 适配 | `apps/desktop/src-tauri` → mmdbridge-core | Tauri command contract / Rust Core 本轮不改 |

基线 `App.tsx` 为 1766 行，68 个 useState 调用（包括缩略图组件 2 个）、17 个 effect、56 个 App 内函数、71 个 invoke 站点。问题是职责与异步生命周期混杂，而非需要替换 React 或 Mantine。

## 2. Existing Interaction Inventory

| 用户任务 / 界面 | 当前入口与行为 | 本轮重点 |
| --- | --- | --- |
| App Shell / Sidebar / Topbar | 左分类/Root/收藏/集合，中搜索与 Library，右 Inspector | 当前上下文、高亮、可见密度 |
| Search | Enter 提交；名称/文件/路径/标签；清除返回无搜索 | 无结果恢复和输入/已提交状态 |
| Library Header / Type Tabs | 标题、在库或已加载计数、全部/模型/动作/场景 | 全局数量与当前结果口径 |
| Quick Filter | 标签最多24项、AND/OR、骨架分类 | 组合条件可见、清除入口 |
| Smart Collection | 平面规则 builder、保存、应用、删除；停用条件保留 | 草稿默认值、旧集合解释、后续原地编辑 |
| Folder Browser / Breadcrumb | Root选择、目录树、包含子目录、sticky、滚动收纳、返回原资产视图 | 独立滚动、长路径、手动展开 |
| Asset Grid / Card | 虚拟网格、130–300尺寸、选择、双击3D、右键菜单 | Thumbnail优先、名字、异常与选中状态 |
| Inspector | 缩略图、元数据、状态、路径、标签、关系、文件操作和预览 | 异步资产归属、高频操作位置 |
| Batch Selection | 逐卡多选；收藏、标签、移动、PMX删除、包回收 | 明显模式、明确范围、清空和选择已加载 |
| Root Management / Add Root | 类型/路径/名称/递归、添加扫描、改名、停用、移除索引 | 不把移除索引表述为删除素材 |
| Scan Queue | 阶段/进度/数量，排序、暂停/继续/停止 | 可观察状态、操作反馈 |
| Thumbnail Jobs | 底栏摘要，最近12项取消/重试 | 对应资产、失败定位、英语状态 |
| Settings | 动作预览模型、重生成、阶段并发、数据库整理 | 就近错误反馈和配置分组 |
| Operation Journal | 最近100项、完成/失败/需检查，恢复核对 | 明确人工恢复边界 |
| Context Menu | 预览、Reveal、目录、收藏、重生成、删除模型 | 破坏性层级、键盘入口 |
| Confirm / Prompt / Operation Plan | 异步确认输入、Core完整计划、路径/依赖/警告、canExecute | 执行中不失去上下文、错误可见 |
| Startup | 六阶段、耗时、停滞提示、失败日志路径 | 信息真实，不偷做全库重绘 |
| Empty / Error / Notice | 首库引导、搜索无结果、主区域 Alert | 区分空库、空目录、收藏和筛选无结果 |
| Model Viewer | PMX/场景、权重/SDEF诊断、骨骼/视图控制 | 专业密度、头部/面板一致性 |
| Motion Viewer | VMD播放、时间轴、速度、相机模式；VPD缩略图回退 | 保留能力说明，不承诺VPD实时播放 |

## 3. Existing Visual System

优势：深色面板与 Mint 品牌已有一致方向；类型使用克制的 Mint/Violet/Amber；可访问控件和浮层已迁至 Mantine；三栏边界、CSP nonce、小窗口工具栏、弹窗安全边距已有实现。

不足：theme 的 dark/font/radius 与 CSS 变量分别维护；MotionViewer 还有独立颜色/圆角/阴影；低优先文字 `#64737f` 对 panel `#131c25` 约 3.52:1，不适合多数小字号说明；普通卡片有 stagger 入场、位移/缩放/阴影及内部 blur 动画，虚拟滚动反复挂载时会重复播放。

设计令牌目标：Surface(background/sidebar/panel/elevated/overlay/hover/selected)、Text(primary/secondary/tertiary/disabled)、Accent(mint)、Semantic(success/warning/error/info/scanning/stale/missing)、Spacing、Radius、Border、Shadow、Control height、Typography。先用 theme 单一来源和 CSS alias 收敛高频变量；不要逐个重写所有专业可视化颜色。

## 4. Problems

优先级按用户后果安排：P1 日常流程可靠性，P2 效率/信息理解，P3 架构与后续扩展。以下基线定位后续随代码变化应按函数/选择器查找。

| ID | 优先级 / 规模 | 问题与证据 | 具体改法 / 验收 |
| --- | --- | --- | --- |
| GUI-01 | P1 / S | A→B选择时仍保留A标签/关系；App selected effect 692–713；移除标签用当前selected.id | 关联数据携带asset ID，仅显示当前资产结果；加载期间禁用相关变更；延迟A响应不能写入B |
| GUI-02 | P1 / S | 文件计划执行时取消仍可关闭；App execute / operation-modal-actions | 取消与关闭遵循busy；失败原因在计划弹窗内可读；模拟执行中不能关闭，不调用真实Core |
| GUI-03 | P1 / M | 收藏/标签/后台更新调用refresh替换首批；深页选择丢失；refresh 429–537、favorite/tag handlers | 确认后以现有refreshAsset为基础区分局部更新和查询失效；保留页/选择/scroll；过滤移除需单独验收 |
| GUI-04 | P2 / S | 标签/骨架/集合/收藏无结果仍引导扫描和添加Root；empty-state 1647–1654 | 按无Root/查询失败/筛选/收藏/目录/未扫描分类；清除条件、返回库或扫描队列为对应主操作 |
| GUI-05 | P2 / S | cardStatus field草稿为空，但select表面显示CardValid；changeFilterField 1358、builder | 默认值与可见选项一致；未手动变更状态也可保存有效条件 |
| GUI-06 | P2 / S | 侧栏Library在收藏/集合时同时高亮；导航1485 | current上下文唯一；追加aria-current |
| GUI-07 | P2 / S | 存储进度固定42%；就绪只看busy；sidebar-bottom1526 | 移除假进度，用真实数量与已有任务/错误状态 |
| GUI-08 | P2 / S | CardBroken/CardMissing/无图合并成预览待生成；ParseFailed/MissingSource不在卡片体现；1642 | 原状态本地化，只呈现Core返回状态；异常优先，资源卡问题有区分；不更改后台判定 |
| GUI-09 | P2 / S | Inspector显示CardStale等内部枚举；1684 | 与卡片共用可理解状态文案，完整状态保留到详情 |
| GUI-10 | P2 / S | 批量缺少选择已加载和清空；1595 | 增加当前已加载范围选择/清空，不伪称全库全选；保留Core批量安全计划 |
| GUI-11 | P2 / S | Settings/Journal/Plan异常藏在遮罩后的主页面Alert | 在当前modal内提供同一错误，不增加业务调用；错误不靠关闭窗口查看 |
| GUI-12 | P2 / S | 缩略图加载中没有提示；卡片/收藏辅助符号无名称 | 静态加载文案；卡片accessibility name包括名称/类型/重要状态，收藏明确label |
| GUI-13 | P2 / S | 卡片标题单行截断、hover位移和昂贵装饰 | 固定两行标题区域、保留full title；hover仅边框/背景，不改几何尺寸；禁用卡片入场与blur动画 |
| GUI-14 | P2 / S | Text与Viewer颜色双轨，小字对比不足 | theme tokens为单一来源，CSS alias；提高次级文字对比；Viewer只改外壳，不改渲染 |
| GUI-15 | P2 / M | 1040内容区约585px，1250断点左右栏变宽后中心反而骤窄 | 确认后统一Sidebar/Inspector/Toolbar breakpoint；当前小修保留既有几何 |
| GUI-16 | P2 / M | 深滚动手动展开目录被下一次scroll自动收纳覆盖；collapse helper734与onScroll1547 | 后续定义手动覆盖/自动恢复时机；避免直接重写最近collapse机制 |
| GUI-17 | P2 / M | Job仅最近12条、英文状态，无全任务列表/资产定位 | 确认后独立JobCenter，复用jobs_list/summary；需要分页能力时另议API适配 |
| GUI-18 | P2 / M | Smart Collection无法原地编辑，builder仅平面组 | 复用已有Core filterId更新能力，定义支持层级/旧复杂表达式保留；不悄然扁平化 |
| GUI-19 | P2 / M | Inspector高频3D/Reveal位于底部，危险文件操作在标签前 | 确认后Overview高频操作置顶，危险区末尾；保留plan→confirm→execute |
| GUI-20 | P3 / M | App一处承担查询/监听/选择/操作/所有弹窗，CSS新旧覆盖较多 | 按第8节组件边界逐步抽取，首阶段等价行为；不按行数机械拆文件 |
| GUI-21 | P2 / 待验 | keyboard context menu、Tab顺序、modal恢复焦点、DPI缩放 | 离屏验证可见focus/menu/dialog；原生WebView2/屏幕阅读器仍需人工 |
| GUI-22 | P2 / 待验 | 真库10k–50k吞吐、缩略图IPC、快速切换Inspector | 合成data只证明虚拟DOM边界；不得表述为真实50k性能通过 |
| GUI-23 | P2 / S | 返回资产视图的快照不含标签/AND-OR/骨架；AssetViewState、enterFolderView/returnToAssetView | 后续补旧localStorage兼容默认值，验收组合筛选往返，不在本轮改变导航状态模型 |
| GUI-24 | P2 / S | 任务/扫描setInterval没有in-flight保护；App background effects | 后续给现有effect加请求互斥，延迟IPC时同路不重叠；不扩大为新调度器 |
| GUI-25 | P2 / M | Settings所有读取Promise.all，一项失败阻止整个设置打开 | 后续先开窗口、分项加载/失败恢复；保持保存参数与Core并发范围 |
| GUI-26 | P2 / S | Inspector元数据依枚举顺序只取前10项；App metadata rendering | 后续按类型固定摘要顺序与完整信息展开，保留Core原字段 |

## 5. UX Problems

关键断点是“当前上下文”和“下一步操作”不一致。用户筛选后没有结果，应恢复筛选；收藏为空应返回Library收藏资产；当前目录无资产应查看父目录或包含子目录；读取失败应重试而不是展示首次使用引导。

状态要回答“是什么问题、对当前任务有什么影响、哪里可以处理”：解析失败→检查详情/重新扫描，源缺失→检查路径/索引，卡片缺失→创建，卡片过期→刷新，损坏→校验/重生成。UI仅翻译状态，不模拟Core的可执行判定。

批量模式必须明确与单选不同，并说明选择范围；查询改变后的清空保持现有行为。不能为快速修复引入“全库选择”或让默认双击继续在批量状态打开Viewer。

Settings应按预览模型、缩略图维护、并发、高级数据库维护分组。全库重生成保留明确确认与队列，不挪到启动自动执行。Journal中的人工核对不包装成自动Undo。

## 6. Architecture Problems

请求生命周期与表现逻辑混在App中，导致旧详情归属、模态错误位置和全量refresh难以分别验证。前端应统一asset types/IPC调用包装，但不能把Core的发现、过滤、关系、操作安全搬到JS。

持久LocalStorage、asset-view scroll恢复、Folder collapse和Virtuoso customScrollParent属于产品契约；抽取hooks时必须保留所有权和effect依赖。不要创建第二套目录逻辑、Thumbnail生成器、全库过滤器或Viewer。

## 7. Design Direction

目标是让资产更显眼、动作更清楚、反馈更可信。保留深色/Mint和当前真实Thumbnail，不增加独立UI库。

卡片层级：Thumbnail → 两行名称 → 类型与重要状态 → 已有API能提供的少量信息。当前compact list不含标签时，不为每张卡片新增asset_tags IPC；Primary Tags必须先定义有界列表数据适配，随后再做。

Shell维持左导航/中浏览/右详情；Search是持续入口，Primary action随上下文变化。Scan与Jobs放在任务区域，Journal留为二级操作。Inspector采用Overview、Metadata、Tags、Relations、Source、Card/Thumbnail、Actions；Move/Rename/Delete置于低优先危险区。

动画只保留必要开合、选择、进度反馈；高频虚拟卡片避免入场、blur、shadow动画，继续支持prefers-reduced-motion。

## 8. Component Map

以下是**待确认的Phase1等价抽取方案**，不是本轮新增组件：

| 边界 | 建议模块 | 归属 / 不允许变化 |
| --- | --- | --- |
| 应用骨架 | App、LibraryShell、LibraryNavigation、LibraryToolbar | App只协调feature；导航语义与尺寸先保持 |
| 查询与选择 | useLibraryQuery、useAssetSelection、shared types | 120 cursor page / revision / selected ID / 局部refresh / scroll保持 |
| 目录浏览 | FolderBrowser、useFolderNavigation | Core目录结果、独立scroll、collapse、返回资产状态 |
| 卡片 | AssetGrid、AssetCard、CardThumbnail | Virtuoso customScrollParent、stable key、四路队列、URL清理 |
| 详情 | AssetInspector | asset ID归属、标签/关系、预览和Reveal |
| 筛选 | LibraryFilters、SmartCollectionBuilder | Core表达式原样传递；旧停用条件不恢复 |
| 后台 | useLibraryJobs、ScanQueueDialog、JobCenter | listener/poll清理，任务状态源仅Core |
| 文件操作 | useAssetOperations、AssetOperationDialog、OperationJournalDialog | plan/confirm/canExecute/busy/结果与恢复边界 |
| 配置 | SettingsDialog、AddRootDialog | 现有IPC、并发范围和取消语义 |
| 共用浮层 | 现有LibraryDialog / LibraryContextMenu | 直接复用，不重复制造 |
| Viewer | 现有ModelViewer / MotionViewer / viewerRendering | lazy加载、canvas挂载和资源释放原样保留 |

实施依赖：先固定types/表现状态 → 等价抽取Shell/卡片/详情 → 查询与任务hook → 独立dialogs → 去除已无引用的CSS → 最后调整IA/Viewer外观。每批保留可单独回退的差异，先编译再选相关离屏回归。

机械抽取与行为改造必须分别review：第一批只移动现有代码，不能借抽取顺手改变分页/scroll语义；GUI-03局部refresh与浏览保真、GUI-17新JobCenter在等价抽取稳定后另批实施。表内这些名称是最终组件归属，不表示全都是等价移动。

## 9. Responsive Rules

| 窗口 | 当前基线 | 后续规则 / 验收 |
| --- | --- | --- |
| 1040×680 | 约205侧栏、250详情、585中央；Topbar两行 | 两列卡片仍可用；动作换行；三滚动区独立；modal footer可滚动到达 |
| 1250×780 | lg切换侧栏248 / 详情304，中心约698；工具栏边界 | 同时比较1250附近断点，中心不得随加宽突然不可用 |
| 1440×900 | 常规三栏，卡片尺寸滑块调整密度 | 标题/动作不挤压搜索和卡片，长中日文允许截断并查看全文 |
| 1920×1080 | 常规三栏，四列以上取决于卡片尺寸 | 中央宽度用于更多资产，不无意义放大导航/Inspector |

统一规则：所有flex/grid文本容器min-width:0；名字两行固定区域，路径合理wrap或ellipsis并有全文title；菜单/下拉/Viewer不横向溢出；主页面不产生横向滚动；modal正文滚动不隐藏确认/取消。130/300卡片尺寸都要复查。DPI与200%缩放属于待实机验收，1040 CSS px通过不能替代。

## 10. Acceptance Criteria

1. Rust Core / Tauri command / query semantics / 依赖不变，没有源资产或数据库写入验证。
2. 当前资产只能显示自身tags/relations；延迟与失败请求不给其他资产提供变更入口。
3. 六类空状态有正确恢复路径；筛选无结果不主推添加Root。
4. CardMissing/Stale/Broken、ParseFailed、MissingSource、加载/无图有可理解区别。
5. 选择/批量/收藏有名称和可见状态；批量选择已加载明确不代表全库。
6. 文件计划执行时所有关闭路径均阻止；失败原因在当前弹窗可见。
7. theme与主要CSS颜色/字体/圆角来源统一；卡片滚动不触发入场和昂贵hover特效。
8. 1040/1250/1440/1920、长中文/日文/路径、空库和异常状态无unexpected horizontal overflow。
9. Virtuoso / lazy thumbnail / Viewer lazy load / Folder collapse / scroll restore仍存在；合成10k/50k仅挂载有界卡片DOM。
10. 最终源码TypeScript/Vite与便携EXE编译通过，EXE大小/时间对应；无SHA校验、无安装器、无新分支/commit/push。

## 11. Regression Checklist

| 范围 | 最小回归 | 证明范围 / 限制 |
| --- | --- | --- |
| 构建 | npm run build；最终npm run tauri -- build --no-bundle | 类型与生产打包，非实机稳定性 |
| 主浏览 | model/motion/scene/favorites/saved filter/search/tag/folder/bulk | 本轮模拟IPC请求与界面状态，非真实Core查询 |
| 状态 | startup、无Root、筛选无结果、空目录、无图、失效/失败状态 | 合成状态本地化与CTA、非真实素材解析 |
| 详情并发 | A详情延迟→切B→B结果→A返回；失败后retry | 只证明前端响应归属与变更入口 |
| 操作弹窗 | blocked plan、模拟执行busy/失败 | 不执行Move/Delete，不能证明Core文件安全 |
| 二级界面 | Scan、Jobs、Settings、Journal、Add、Prompt、Confirm、Context | 可见布局/取消/键盘基础，不等于全部业务写入 |
| Viewer | Model/Scene/Motion离屏合成网格挂载 | 专业控制布局，非真实PMX/VMD视觉对照 |
| 响应式 | 四宽度 + 130/300 card | 无横向溢出与不可达操作，非Windows全DPI |
| 大库 | 真实合成数组10k/50k，mounted DOM数量 | 虚拟化有界，非数据库/IPC/GPU性能基准 |
| 最终review | git diff --check / diff文件分类 /依赖与旧代码检查 | 只保留本轮前端与文档变化，未提交未推送 |

## 本轮离屏步骤与基线证据

截图来自本轮已构建源码。所有截图保留在忽略目录；Git只存报告。常规布局夹具48条、在库数字8536；10k/50k扩展使用实际合成数组，必须分开解释。

| 步骤 | 本轮截图 / 健康状态 | 审查结果 |
| --- | --- | --- |
| 1 主Library四宽度 | `baseline/01-library-1480.png`、02/27/19及1440补图；布局可用 | 三栏已成立，1250以下头部占高；假42%可见 |
| 2 卡片与Inspector | `baseline/03-inspector-1040.png`、04；需改进 | CardStale内部名、长名称截断、3D位于底部 |
| 3 标签与组合筛选 | `baseline/06-tags-1040.png`、07；需改进 | 换行可用，空结果恢复和默认状态问题需定向用例 |
| 4 Scan / Journal / Add | `baseline/08-scan-1040.png`、09/10；布局可用 | Core安全入口已有，不重造业务流程 |
| 5 菜单/缩略图/批量 | `baseline/11-context-1040.png`、12/13；需改进 | 破坏性菜单层级、选择范围说明可补 |
| 6 Folder展开/收纳 | `baseline/14-folder-1040.png`、15；布局可用 | 保留独立目录滚动，手动收纳覆盖待后续设计 |
| 7 Model/Scene/Motion | `baseline/16-model-1040.png`、17/18；合成布局可用 | 不将三角网格解释为真实MMD验证 |
| 8 Startup / Empty | `baseline/20-startup-1040.png`、20-empty；需改进 | 启动反馈已有，其他空上下文未细分 |
| 9 Prompt / Plan / Confirm | `baseline/21-prompt-1040.png`、22/23；需改进 | blocked plan禁执行正确；busy关闭漏洞是源码确认 |
| 10 Settings长内容 / Card尺寸 | `baseline/24-settings-bottom-1040.png`、25/26；布局可用 | footer可达，设置失败需就近反馈 |

图片根目录：`E:/MMD/MMDbridgeLib/target/gui-redesign-audit/`。下面的交付记录说明本轮已完成的首批改动；不能据此宣称Phase1–10完整重构已完成。

### Phase 0 完成记录

产品代码修改前，已完成本轮冻结源码的49个离屏截图状态。`baseline/suite-metrics.json`、`baseline/extended-metrics.json`、`baseline/scenario-metrics.json`及`baseline/focused-metrics.json`记录页面错误、CSP、三栏边界与横向溢出；路径均相对于`target/gui-redesign-audit/`。本轮没有发现这些布局错误。

新增证据包括1440常规窗口、6类异常卡片与详情、搜索/骨架筛选无结果、空文件夹、10k/50k合成数据上/中/下滚动位置。两个大数据夹具的数组、mock assets_page返回和Virtuoso data长度均分别为10,000/50,000；1440×900时卡片DOM上/中/下为24/36/20。模拟接口特意整批返回，**不是**120条真实Core分页验证，也不是性能基准。

已查看关键原图，确认：ParseFailed/MissingSource未在卡片显示；CardMissing/Broken/无图共用“预览待生成”；筛选与空目录仍要求扫描/添加Root；快速滚动中新挂载卡350ms仍处于淡入，950ms稳定。后续小修应直接对应这些证据，保留真实缩略图与既有Fallback设计。

关键基线图：[三栏常规窗口](../target/gui-redesign-audit/baseline/30-library-1440.png)、[异常卡片](../target/gui-redesign-audit/baseline/33-status-cards-1440.png)、[解析失败详情](../target/gui-redesign-audit/baseline/34-inspector-parsefailed-1040.png)。截图在忽略目录，跨机器阅读本报告时图片可能不可用。

### Phase 1–10 后续交付界限

| Phase | 本轮处理 | 需要确认后继续的完整范围 |
| --- | --- | --- |
| 1 前端架构 | 只修正详情响应归属，无整页拆分 | 按Component Map等价抽取，再单独处理深页刷新保真 |
| 2 Design System | 主语义token收敛、卡片动效与文字层级 | 清理所有遗留覆盖，专业可视化色阶单独验收 |
| 3 信息架构 | 正确导航高亮、真实侧栏状态 | Shell断点/Topbar主次入口和重复导航收敛 |
| 4 Card/Grid | 重要状态、两行名称、加载反馈、虚拟化保留 | Primary Tags有界列表摘要API适配，再完善processing状态 |
| 5 Inspector | 标签/关系ID归属、状态本地化、就近错误/重试 | Overview高频动作置顶、固定元数据摘要、危险区末尾 |
| 6 Workflows | 空结果恢复、选择已加载与清空、筛选默认值 | 旧集合原地编辑、嵌套表达式兼容、目录状态往返/手动覆盖 |
| 7 Secondary UI | 执行中关闭一致、当前Modal可见错误 | 完整JobCenter、日志需检查提醒、分项Settings加载 |
| 8 Viewer | 仅共用外壳token，不触及3D逻辑 | Header/panels/controls/timeline分阶段一致性，保留专业密度 |
| 9 Polish | 卡片名称/状态语义、focus、reduced-motion、无昂贵卡片动画 | 原生WebView2/DPI/屏幕阅读器/真库性能，按风险选择用例 |
| 10 Regression | 本轮最终源码build、离屏范围、便携EXE、diff review | 后续每批只跑受影响回归，全部改造后再最终人工验收 |

本轮首批小修完成不表示整个GUI重构已完成。较大范围继续执行前，需按用户“如果需要大改动需要向我确认”的要求确认上表；不因本文件存在就自动授权这些改造。不创建新分支、不提交、不推送。

## 本轮最终交付记录

### 已实施的产品与架构改动

1. **当前资产信息可信**：标签/关系状态增加asset ID、revision及loading/ready/failed归属。选择变化立即隐藏旧信息，过期响应不进入当前Inspector；相关变更入口加载时禁用，失败后就近重试。不是整页架构拆分。
2. **文件操作不丢失上下文**：执行中的取消、关闭、Escape和外部点击统一锁定；执行错误留在计划弹窗内。Settings、Operation Journal使用各自错误反馈，不要求先关闭弹窗才能看到错误。
3. **浏览上下文与恢复一致**：Library/收藏/集合高亮互斥；侧栏删除固定42%假存储进度，使用已有数量、扫描及任务状态。空Root、读取失败、筛选、收藏、空目录、未索引上下文采用对应恢复操作；清除查询条件保留目录/Root/类型范围。低高度窗口减少装饰与留白，使空库和搜索/筛选恢复按钮进入首屏。
4. **状态与选择可理解**：解析失败、源缺失、卡片有效/缺失/需更新/损坏、无图与读取中分别提示；不更改Core判定。批量模式增加“选择已加载（N）”与清空，CardStatus默认条件与可见值一致，任务状态中文化并优先显示已加载资产名。
5. **高频视觉收敛**：theme单一token来源经Mantine CSS variables resolver输出，旧CSS变量作为alias；保留CSP nonce。次级小字对比提高，卡片固定两行名称，去除入场延迟、hover位移/缩放/阴影与卡片装饰blur动画；hover仅改变边框/背景。Viewer只统一外壳token。
6. **基础可访问性**：卡片、收藏和详情名称补充可访问名称/full title；卡片Enter预览与键盘菜单入口复用既有行为，提供2px focus ring和reduced-motion支持。未宣称满足完整WCAG或屏幕阅读器验收。

GUI-01/02/04–14为首批主要修复；GUI-17只完成既有任务列表文案与名称，完整JobCenter仍待确认。其他问题按第4节和Phase表继续，未改变Rust Core、Tauri IPC、120条分页合同、真实资产处理或3D算法。

### 文件与组件变化

| 文件 | 本轮职责变化 |
| --- | --- |
| `apps/desktop/src/App.tsx` | 详情归属/重试、局部错误、busy关闭保护、状态文案、导航/空态/批量/键盘入口 |
| `apps/desktop/src/theme.ts` | libraryTokens和CSS variables resolver，Mantine与产品CSS共用语义来源 |
| `apps/desktop/src/main.tsx` | Provider注册resolver，保留nonce和启动流程 |
| `apps/desktop/src/styles.css` | token alias、卡片与空态、focus和动效，删除无引用假存储/动画样式 |
| `apps/desktop/src/mantine-layout.css` | 移除重复AppShell grid覆盖，使用共用token，保持现有断点宽度 |
| `apps/desktop/src/virtualized-grid.css` | bulk静态状态与有界卡片样式，保留Virtuoso布局 |
| `apps/desktop/src/model-viewer.css` | 外壳颜色/文字/圆角token |
| `apps/desktop/src/motion-viewer.css` | 外壳颜色/文字/圆角token |
| `docs/GUI_REDESIGN_V1.md` | 本次审查、设计规格、分批计划、验收及交付记录 |

没有新增产品组件，没有移除业务组件，没有新依赖；复用LibraryDialog/LibraryContextMenu和已有Viewer。没有为了拆分行数创建第二套状态或UI框架。上述8个产品文件为未暂存修改，本报告为新增未跟踪文件；未创建分支、未提交、未推送。夹具、截图、冻结dist和检查记录仅在忽略目录`target/gui-redesign-audit/`。

### 离屏审查步骤最终状态

| 步骤 | 健康状态 / 当前证据 |
| --- | --- |
| 1 主Library四宽度 | 布局通过；导航/状态已修。1040/1250/1440/1920及原1480夹具无横向溢出，断点策略仍待优化 |
| 2 卡片与Inspector | 首批修复通过；`after/33-status-cards-1440.png`与34–39详情。异步归属/重试定向检查通过，详情结构仍待重组 |
| 3 标签与组合筛选 | 当前布局和请求合同通过；默认CardValid保存通过。集合原地编辑与复杂表达式能力待后续 |
| 4 Scan / Journal / Add | 当前布局通过；就近错误已补。未验证真实Root写入/扫描/日志恢复 |
| 5 菜单/缩略图/批量 | 当前布局通过；破坏性项颜色、静态加载、已加载选择范围已改。未批量写真实资产 |
| 6 Folder展开/收纳 | 既有布局保留；空目录恢复操作滚动后可达。手动展开覆盖与筛选往返问题仍待改 |
| 7 Model/Scene/Motion | 合成网格布局通过；只改外壳token，真实PMX/VMD和GPU效果待人工 |
| 8 Startup / Empty | 当前离屏状态通过；`after/20-empty-1040.png`、40/41恢复按钮首屏可见，46目录按钮滚动可达 |
| 9 Prompt / Plan / Confirm | 当前布局和busy/失败保护定向检查通过；只模拟计划与执行失败，没有真实Move/Delete |
| 10 Settings长内容 / Card尺寸 | 当前布局通过；130/300卡片与modal footer可达，设置分项加载仍待改 |

最终保留43张不同状态的after截图：首轮42个截图状态，加上4个受影响空态补查，其中3张覆盖原图、1张新增。验证数据为`after/suite-metrics.json`（26状态）、`scenario-metrics.json`（2状态）、`extended-metrics.json`（14状态）、`empty-recheck-metrics.json`（4状态）及`regression-metrics.json`（23项不同定向检查）。最后一轮只重查低高度空态，没有重复执行全套。

最终记录没有page/CSP error、横向溢出或三栏边界异常。定向检查覆盖详情A晚于B返回、加载期变更禁用、失败重试、blocked plan、busy全部关闭路径、局部失败反馈、类型/收藏/集合/搜索/标签请求参数、默认资源卡条件、focus/token注入。两个显式写入调用均由页面mock模拟（filter_save与故意失败的asset_operation_execute），没有真实数据库或文件操作。审查器曾误把只读tags_list算作标签写入；已根据记录中的命令白名单纠正分类并保留原记录，不将该误报记为产品缺陷。

目录首次单次滚到底部后会触发既有收纳/高度变化，按钮当时可能仍被底栏遮挡；补查等待布局稳定并继续滚到恢复操作后通过。它只证明操作可达，不能证明GUI-16收纳体验已改善。

### 性能、构建与便携交付

基线10k/50k合成数组已确认Virtuoso data长度与返回数组一致；最终50k中段夹具挂载32张卡片，保持有界DOM。此证据不覆盖真实Core分页、吞吐、IPC/缩略图耗时、FPS或内存峰值。卡片动效成本降低属于实现变化，未测量具体性能增益。

最终生产包（Vite十进制kB）App JS 233.69、入口JS 395.68、主CSS 288.48；基线分别229.38、392.12、288.62。App约增加4.31kB、入口约增加3.56kB；没有以打包体积下降宣称性能改善。ModelViewer 514.69kB的既有大chunk警告仍存在，lazy import保留，未为消除警告引入新打包策略。

最终`npm run build`（TypeScript + Vite）和`npm run tauri -- build --no-bundle`均成功；Rust Release编译44.67s。未生成安装器，未启动桌面UI。便携EXE已同步，源/目标的大小与修改时间一致，未做SHA校验：

- 源：`E:\MMD\MMDbridgeLib\apps\desktop\src-tauri\target\release\mmdbridge-desktop.exe`
- 交付：`E:\MMD\MBL\mmdbridge-desktop.exe`
- 大小：24,005,632 bytes；修改时间：2026-10-01 21:50:35（Asia/Shanghai）。CLI本轮未改、未重建。

真实WebView2运行、Windows DPI/200%缩放、屏幕阅读器、真实PMX/VMD/场景、真库10k/50k分页性能、扫描/任务/设置写入和真实文件计划执行尚未验证。源码保留Folder与Viewer既有机制不等于这些实机交互已经验收。

后续建议依次确认并推进：等价抽取Shell/卡片/Inspector → 查询与选择保真（GUI-03/23/24）→ Inspector高频动作/摘要（GUI-19/26）→ 集合/JobCenter/设置与Viewer一致性。每批只运行对应最小回归；不一次性重写App，也不更改Core。待用户确认后，按第8节Component Map和对应验收执行。

当前对照图：[异常卡片](../target/gui-redesign-audit/after/33-status-cards-1440.png)、[小窗筛选恢复](../target/gui-redesign-audit/after/41-empty-filtered-1040.png)、[空目录操作可达](../target/gui-redesign-audit/after/46-empty-folder-bottom-1040.png)。这些是明确的离屏夹具，不是真实Library截图。
