# 全库重扫、缩略图与标签工作（2026-10-01）

范围：便携版 `E:\MMD\MBL` 中已启用、递归扫描的模型、动作和场景根。使用共享 Core CLI；不启动桌面窗口、不修改源素材、不进行 SHA 校验、不清空数据库或用户覆盖记录。

数据库备份：`E:\MMD\MBL\data\backups\library-before-20261001-reindex.sqlite3`。
运行记录与审阅页：`E:\MMD\MBL\data\runs\20261001-reindex`。

## 最终结果（2026-10-01 18:25 北京时间）

状态：已完成全范围重扫、缩略图处理、skill 审阅打标及写后核验；异常资产明确跳过。扫描三根目录共 8,986 个文件，普通库存 8,536 个；450 个纯镜头文件作为辅助数据保留，不按内部 ID 打标。

| 类型 | 普通库存 | 有效缩略图且已审阅 | 异常跳过 | 新增标签 |
| --- | ---: | ---: | ---: | ---: |
| 模型 | 3905 | 3753 | 152 | 25349 |
| 动作 | 2636 | 2622 | 14 | 3937 |
| 场景 | 1995 | 1978 | 17 | 2165 |
| 合计 | 8,536 | 8,353 | 183 | 31,451 |

- 新增 31,451 条标签，657 条已有同义候选跳过；没有 Core 用户覆盖拒绝、清单同步失败或写后核验失败。本轮范围原有删除覆盖记录为 0，未添加或删除覆盖记录。
- 实际影响 6,516 个资产，全部清单已通过 Core 同步并直接核验为 CardValid、hasThumbnail=true。原有 25,568 条标签记录的名称、来源和置信度均保留；受影响卡片的 preview.webp 与实际审阅导出逐字节一致，没有 SHA 校验。
- 3,493 项未追加视觉标签（模型 49、动作 2,622、场景 822），只使用已有标签和解析信息支持的候选；单帧动作预览不能证明舞蹈风格或速度。
- 183 项异常包括 119 项解析失败、64 项缩略图失败。后者为引用越界 32、骨骼层级环 1、非有限骨骼初始位置 1、Morph 环 1、无可渲染三角网格 15、非有限蒙皮顶点 14；保留错误和资产 ID，未改动源素材。
- 本轮已恢复直接自引用骨骼的 41 个模型，并通过现有带 ID 文件名机制恢复 3 个发布冲突资产。修复 JSON 浮点往返造成的 726 张过期清单，预览保留。
- 当前源代码 CLI Release 与桌面 --no-bundle 编译成功并已部署。正式 CLI 13,355,008 字节，桌面 EXE 24,003,072 字节，源/目标时间与大小一致；临时 observer 已清理。未启动桌面 UI 验证，未制作安装包，未提交或推送。

最终 Core 审计：`E:\MMD\MBL\data\runs\20261001-reindex\final-completion-audit.json`。写入报告与原标签快照：`retag-plan-applied-20261001-165752.json` / `retag-plan-applied-20261001-165752-baseline.json`。完整异常清单：`final-terminal-failures.json` 与最终各类型库存。

以下按时间保留执行与诊断阶段记录。

## 扫描队列最小修复

发现普通重扫会继承旧终态任务的 `full_check`：入队 SQL 始终取新旧标志的最大值，使过去的深度检查永久影响后续扫描。

`scan_queue.rs` 改为在 Completed / Failed / Cancelled 上使用本次请求的标志；进行中、暂停或等待中的任务仍保留原有升级语义。本轮已经开始的模型扫描保持运行，后续扫描使用新 EXE。

验证：`cargo test -p mmdbridge-core ordinary_rescan_clears_previous_terminal_full_check --locked --offline -- --nocapture`，1 项通过，覆盖三种旧终态；测试本身耗时 0.09 秒。CLI Release 离线构建成功。`npm run tauri -- build --no-bundle` 成功，两种 EXE 已部署至 `E:\MMD\MBL`，以时间和大小核对复制结果；未启动桌面窗口。CLI 在模型扫描退出后替换。完整任务结果待实际完成后补充。

## 本轮执行状态

### 零四元数兼容处理

本轮模型队列中真实 2B 资产（`c122a66d-f98b-4376-91fb-4e7a2d3758a4`）因骨骼 Morph 的 rotation_offset 为零四元数而无法构建运行时。增加共享 `pmx_runtime.rs` 小型适配：正常导入成功时原样返回；仅在骨骼 Morph 四元数验证错误时，复用现有 PMX 解析器与导出器，在内存中把严格全零旋转换为单位旋转后再次导入。其他无效数据继续返回错误。缩略图及动作预览共用该适配，缩略图报告记录修正数量。原 PMX 保持原样。

CLI Release 构建成功。真实共享库单资产重试先遇到 DatabaseError（database is locked），第二次任务为 Failed（thumbnail job was cancelled）；源码进度回调会把数据库错误转换为中止信号，因此不能把 CLI 的退出码 0 当作渲染成功。原批量队列继续运行，后续实际库重试等竞争写入结束再进行。

通过既有 Core `Library::render_thumbnail_file` 和内存库执行隔离只读验证：`cargo run -p mmdbridge-core --release --locked --offline --example render_thumbnail -- <2B原文件> <唯一输出.webp>` 成功；RTX 4090 Vulkan，101,015 顶点、158,474 三角形、36 材质、13 贴图，报告 `ZeroBoneMorphQuaternionAsIdentity:22`。1024×1024 WebP 已目视检查为正常完整人物取景。报告和图片保存在运行目录 `2b-zero-quaternion-probe.*`。实际库中该资产的卡片尚待重试；没有验证桌面交互或 VMD 对此模型的播放。原本成功的导入不进入适配分支，渲染版本及当前有效缩略图保留。

补充只读 CLI `scan-status [--root <id>] --json`，复用 Core `list_scan_states()`；它不入队或恢复扫描。原因是 PowerShell 原生命令重定向尚未实时落盘 stderr，只有根状态无法呈现实际文件进度。Release 构建与真实查询成功：动作扫描返回 Indexing、1,120 / 3,086、fullCheck=false、error=null。当前以临时 `mmdbridge-observer.exe` 查询，共享同一便携数据库；流水线结束后更新正式 CLI 并移除临时 EXE。

同样将现有 Core `job_summary()` 接到只读 CLI `jobs summary --json`。Release 构建及真实查询成功。新缩略图队列启动前历史任务基线为 Completed 13,034 / Failed 1,209 / Cancelled 41，已保存至运行目录的 `jobs-summary-before.json`；后续不能把历史任务计入本轮生成结果。

增加 `tags list <id> --include-overrides --json`：默认输出仍为原标签数组，显式选项返回标签和 suppressedTags。Core 复用卡片上下文中已有的覆盖记录查询，没有改变标签写入、覆盖或数据结构。CLI Release 构建、真实只读查询及工具语法检查通过。打标工具会在候选计划及写入前检查精确名称、旧同义名称和可对应的未加前缀名称，防止重新添加用户移除过的同义标签；实际写入仍由 Core 判定最终用户覆盖。

- 初始可见库存：模型 3,905、动作 2,636、场景 1,995。模型 38 项、场景 2 项原标记为 ParseFailed。
- 初始符合当前版本的卡片：模型 3 张，动作与场景均为 0；缺失或过期卡片将在扫描结束后由 Core 队列处理。
- 模型扫描已于 2026-10-01 11:19（北京时间）达到 Completed，3,905 / 3,905，error=null。修复后的 CLI 随后部署至 `E:\MMD\MBL`。
- 动作扫描已于 2026-10-01 11:35（北京时间）达到 Completed，3,086 / 3,086，error=null、fullCheck=false；重新清点为 2,636 项正常可见动作，全部 Ready，纯镜头可见项为 0。场景扫描已开始。
- 场景扫描已于 2026-10-01 12:02（北京时间）达到 Completed，1,995 / 1,995，error=null、fullCheck=false。三类重扫全部结束，模型缩略图队列已启动。初次队列观察相对历史基线新增完成 83、失败 5，另有 Parsing / Rendering / Encoding 任务；这不是最终生成结果。
- 缩略图生成与重新打标尚未全部完成；初始数量不是最终结果。
- 使用 `skills/mmdbridge-card-manager/SKILL.md` 的当前规则。只为有效卡片补充有依据的标签；保留用户标签、移除覆盖、旧未加前缀标签及冲突标签。写入后仅同步受影响的 manifest，并验证卡片。

### 组合 Morph 无效引用兼容

真实资产 [The Looking Glass] Mew (`d0840134-8995-4ff7-8435-9a0849d79021`) 的完整 PMX 解析及骨架分段读取成功，但运行时导入 SectionOverflow。通过现有 SDK 只读诊断确认：第 96 个“呼吸”组合 Morph 含自身引用 96 和无效引用 -1。仅删除 -1 后，运行时明确报告组合 Morph 环，因此进一步只忽略直接自引用，不处理其他跨 Morph 环。

共享适配只在已知全零四元数、SectionOverflow、组合 Morph 环错误之后尝试现有解析/导出流程；正常导入原样返回。只忽略负数组合引用和直接自身引用，保留有效引用、Morph 序号及名称，不修改源文件。未知错误及修正后仍失败的资产继续报告失败。真实 Tda China Miku (`5faeb9b7-8290-46bf-a7de-8b55ad0bda4e`) 的“素足”第 66 个组合 Morph 也含自身引用 66，另两个引用 67/68 保留。

Mew 隔离 Core 渲染成功，1024×1024 WebP 已目视检查；报告 43,582 顶点、78,558 三角形、22 材质、13 贴图，以及 IgnoredNegativeGroupMorphReference:1 / IgnoredSelfGroupMorphReference:1。诊断、报告和图片位于运行目录 section-overflow-probe.log / mew-negative-group-probe.*。实际库重试仍等待原队列结束；不能把隔离输出当作已入库卡片。预览仍有既有 UnsupportedNonTriangleStyle 提示，本轮没有据此猜测颜色或改动材质。

### 预览审阅准备（阶段记录）

原模型缩略图队列保持运行。使用 Core cards thumbnail / tags list 导出首批当时有效的 1,401 张模型卡片，生成不可混淆的 ID 映射审阅页 model-7d928b1d-000 至 029。已实际目视审阅 000 至 006 共 336 个资产，保存三份视觉候选计划；含空白或不可辨认预览的条目明确不追加视觉标签，服装/头发/身体部件不套用完整人物标签。计划尚未写库，剩余页尚未审阅。后续最终导出必须补齐新完成及修复卡片的覆盖。

CLI 最新 Release 构建成功；桌面 portable --no-bundle 构建成功，2026-10-01 12:53 的 EXE 已部署，源/目标大小均 23,945,216 字节。更新观察 CLI 大小 13,287,936 字节；正式 mmdbridge.exe 仍被原队列使用，等待退出再换。Tda China Miku 的隔离真实渲染成功并目视检查；不代表实际库卡片已修复。

### Incremental visual review milestone (2206 model assets)

All 1401 initial model previews and all 805 previews from the first delta export have been inspected. The ID-to-own-preview and sheet mapping audit passed for 2206 distinct assets. The saved plans contain 12712 visual candidates; 23 previews have no visual candidates because the evidence is insufficient. No tags have been written. Existing tag data and suppression overrides are preserved. The next delta export is running for 778 more cards while the original model rendering queue continues. These counts describe a preparation milestone, not full-library completion.

### First model generation finished; motion generation started

Core returned 3588 completed and 203 failed model card jobs (3791 queued, no cancellations). Fresh Core inventory contains 3588 CardValid cards, 203 Ready assets without cards, and 114 ParseFailed rows. Parser errors comprise 102 invalid PMX headers, one section overflow, and 11 unexpected-end-of-data errors; source files remain untouched. The three previously diagnosed compatibility fixtures are still Ready with missing cards, so canonical retries remain pending until the shared queue is idle. Motion generation started automatically in the same pipeline. Visual review now covers 2984 model assets; the next immutable delta export selected 604 remaining current valid model cards. No tag writes have started.


## 动作 IK 轨道越界修复

动作首轮 2591/2636 成功，33 项 worker panic（pose.rs:189，IK 数组 len 14 / index 14），12 项非有限蒙皮坐标失败。缩略图和动作查看器原先直接使用 VMD 原始 property 数组；现复用 SDK build_pair_clip 按 PMX IK 名称映射求解器槽位。真实失败资产 fa9078c5-aff6-44ff-aac8-1e3afbb820d4 已通过隔离内存库 Core 离屏生成 1024² WebP，并查看图像；不写用户库和源文件。桌面便携版已编译复制，UI 未启动验证。正式 CLI 待场景队列空闲后更新并重试。2591 个当前有效动作预览已全部逐页检查，覆盖核对通过，未写标签。

### 只读批量标签核验入口

新增 CLI `tags audit-batch --asset-id <ids...> --json`，每批最多 100 个资产，直接复用 Core 的 `verify_card`、`list_asset_tags` 和 `list_asset_tag_overrides`。单资产命令保持原输出。

Release CLI 编译通过，已部署到独立 observer；主 CLI 仍等待正在运行的场景生成进程退出再替换。首次对比核验遇到生成期间的 `DatabaseError: database is locked`，结果尚未验证。标签写入脚本只有在 `tag-audit-batch-parity.json` 的 `exactParity=true` 时才启用批量预检和写后核验，目前没有标签写入。

只读批量核验的后续对比已通过：同一组 3 个资产的卡片验证、现有标签和覆盖规则与原命令完全一致，单项命令合计 3.594 秒，批量入口 0.313 秒。主 CLI 已在旧场景进程退出后同步到 2026-10-01 15:14:09 的 Release 构建（13,306,368 字节）。无 SHA 校验。

场景首轮缩略图生成终态为 total=1990、completed=1972、failed=18、cancelled=0；1972 个当前有效场景预览已逐图检查，资产 ID 集合与 Core 当前有效场景集合完全相等。剩余失败项的有界重试仍待进行，尚未写入标签。

## 最终重试与核验进度（16:20 更新）

三根目录扫描已全部 Completed，无扫描错误。初次生成和有界重试均已结束：目前模型 3,750、动作 2,622、场景 1,978 张有效缩略图，共 8,350 张。所有实际 Core 预览均已逐页检查，使用不可混淆的资产 ID 映射；图片不可辨认的条目只保留解析证据候选。此时标签写入仍未开始。

新增兼容仅在 SDK 明确报告骨骼 parent 直接自引用后进入，将自身 parent 在内存中视为根（-1），不改变源文件。真实单资产离屏探针成功并查看图像；实际库 41 项重试全部 Completed，41 张卡片验证通过，新增预览均已检查。

最终直接核验发现极小尺寸浮点值通过 serde_json 默认解析/序列化后末位漂移，导致立即同步后的卡片仍 CardStale。使用原 manifest 的只读往返诊断得到 width 从 1.9485949565023483e-7 漂移为 1.9485949565023485e-7，复现 equality=false。启用现有 serde_json 的 float_roundtrip 功能，针对性元数据往返测试通过。真实问题卡片 48ab69cf-c943-499c-adf7-322c7b0f7e21 经 Core sync-manifest 后立即验证 CardValid、hasThumbnail=true。最终全范围核验会同步并核验同类受影响清单，保留预览、标签和用户覆盖。

最新 CLI Release 与桌面 --no-bundle 编译通过并部署：CLI/observer 13,355,008 字节（16:18:10），桌面 24,003,072 字节（16:19:40）。没有启动桌面 UI，没有制作安装包，没有 SHA 校验或提交推送。

## 全范围写入前核验已完成（16:48）

Core 最终范围：模型 3,905 中 3,753 可用，动作 2,636 中 2,622 可用，场景 1,995 中 1,978 可用，共 8,353 个资产。模型 114 项、场景 5 项 ParseFailed；其余未生成缩略图的模型 38、动作 14、场景 12 项仍明确保留失败状态。本轮扫描的 450 个纯镜头 VMD 为辅助数据，不进入普通资产打标。

最终直接卡片核验发现并修复 725 张 JSON 浮点往返差异清单（另有先行单张验证），每项同步后重新核验 CardValid，原标签对象及 suppressedTags 完全保留。Core 现有带 ID 文件名机制解决了另外 3 项同名卡片发布冲突；3 项全部 Completed，实际预览已补查，不改动其他资产的卡片。

完整预览 ID 映射核验通过：8,353 个资产、24,177 条视觉候选，3,493 项因图像证据不足不追加视觉标签。解析元数据补充 7,931 条技术候选，合并计划为 32,108 条候选。写入仍须检查现有标签/同义名/用户覆盖/20 条软上限；候选数量不等于实际新增数量。已有角色、类型或格式与候选互斥时保留旧标签、跳过矛盾候选并记录。写入流程已启动，终态尚待核验。
