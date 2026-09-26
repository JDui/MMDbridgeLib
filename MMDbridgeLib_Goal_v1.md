# MMDbridgeLib — Goal

设计并实现一个名为 **MMDbridgeLib** 的桌面资产管理软件，用于扫描、预览、整理、筛选和管理本地 MMD 资产库。

主要管理三类资产：

- 模型 Model
- 动作 Motion
- 场景 Stage / Scene

软件定位不是单纯的文件浏览器，而是一个面向大量 MMD 资产的 **Asset Manager / Library Manager**。

整体使用体验参考 **Steam Library**：

- 左侧为资产库、分类、目录、标签和智能集合
- 中间以大尺寸缩略图卡片展示资产
- 右侧或详情页显示完整资产信息
- 支持快速搜索、组合筛选、标签、关系、版本和重复资产管理
- 后台持续维护本地资产索引
- 支持通过 Agent Skill 自动为尚未整理的资产生成资产卡片

---

# 1. 核心技术约束

以下为硬性技术要求：

- Rust
- Tauri v2
- 本地优先
- Windows 10 / Windows 11 为主要运行平台
- 必须正确处理：
  - 中文路径
  - 日文路径
  - Unicode 文件名
  - 超长路径
- 不依赖 Python Runtime
- 不依赖用户安装 MMD、PMXEditor 等第三方程序
- 核心资产扫描、解析、数据库、缩略图生成、队列、卡片生成逻辑必须位于 Rust Core

前端技术可以根据实际工程情况选择。

如果没有现有工程约束，默认优先考虑：

- Tauri v2
- Rust
- React
- TypeScript
- Vite
- SQLite
- SQLx

前端只负责 UI 和交互。

不得把重要资产解析逻辑实现到 WebView / JavaScript 中。

建议整体架构：

```text
MMDbridgeLib

Frontend
    ↓
Tauri Commands
    ↓
MMDbridge Core
    ├─ Asset Scanner
    ├─ Asset Parser
    ├─ Asset Resolver
    ├─ Relation Resolver
    ├─ Duplicate Resolver
    ├─ Thumbnail Renderer
    ├─ Job Queue
    ├─ MMDRCV
    └─ Database
           ↓
        SQLite

同时：

Agent / Codex
    ↓
MMDbridge CLI / Core API
    ↓
MMDbridge Core
```

UI、CLI 和 Agent Skill 必须复用同一个 Rust Core。

不要分别实现三套资产逻辑。

---

# 2. Asset Root

用户可以分别为：

```text
Model
Motion
Scene
```

添加任意数量的 Asset Root。

例如：

```text
Model
 ├─ D:\MMD\Model
 ├─ E:\MMD\Model
 └─ F:\Download\MMD\Model

Motion
 ├─ D:\MMD\Motion
 └─ E:\Motion

Scene
 ├─ D:\MMD\Stage
 └─ E:\MMD\Scene
```

每个 Root 至少记录：

```text
id
asset_type
path
display_name
enabled
scan_recursive
created_at
last_scan_at
scan_status
```

支持：

- 添加
- 删除
- 修改
- 暂停扫描
- 手动重新扫描
- 后台增量扫描
- 文件系统变化检测

删除 Root 不应该删除磁盘上的任何资产。

---

# 3. Asset 基础模型

所有资产统一抽象为：

```text
Asset

id
type
name
root_id
primary_source
asset_directory

metadata

thumbnail
card

tags[]
relations[]

created_at
updated_at
last_seen_at

fingerprint
status
```

Asset ID 必须稳定。

文件改名或路径变化时，在能够可靠判断为同一资产的情况下应尽可能保持 Asset ID。

---

# 4. 模型资产识别

## 4.1 基本定义

模型资产主要以：

```text
PMX
```

为核心。

正常情况下：

> 一个 PMX 所在目录代表一个模型资产包。

例如：

```text
Miku/
 ├─ Miku.pmx
 ├─ readme.txt
 └─ texture/
```

识别为：

```text
Model Asset
Asset Directory = Miku/
Primary Source = Miku.pmx
```

---

# 5. 模型目录容错

现实中的 MMD 目录并不规范，因此不能简单假设：

```text
一个目录 = 一个 PMX
```

必须处理：

```text
散落 PMX
多个 PMX
模型变体
换装版本
旧版本
备份版本
无独立文件夹 PMX
```

例如：

```text
Models/
 ├─ Miku.pmx
 ├─ Miku_Ver2.pmx
 ├─ Luka.pmx
 ├─ texture/
```

Scanner 应先生成：

```text
AssetCandidate
```

然后通过以下信息综合判断：

- PMX 文件名
- PMX 内部模型名
- 文件名前缀
- 文件名相似度
- 模型结构
- 顶点数量
- 面数
- 骨骼结构
- 材质引用
- Texture 引用关系
- 相邻文件
- Directory 层级

区分：

```text
同一模型不同版本
同一模型不同变体
多个独立模型
散落模型
```

当判断置信度不足时：

**不得擅自合并。**

应生成多个 Candidate，并标记：

```text
NeedsReview
```

用户可以在 UI 中手动确认。

---

# 6. 模型 Metadata

当前 Model Card 至少包含：

```text
name
polygon_count
bone_count
thumbnail
tags[]
```

数据库内部可以同时保存更多能够自动得到的信息，例如：

```text
vertex_count
material_count
morph_count
rigid_body_count
joint_count
pmx_version
file_size
```

但这些不是首版 Card 必须展示字段。

其中：

```text
polygon_count = index_count / 3
```

---

# 7. 动作资产识别

动作主要读取：

```text
VMD
VPD
```

需要解析 VMD 内部数据，而不是只通过扩展名判断。

至少识别：

```text
Bone Motion
Morph Motion
Camera
Light
IK
Pose
```

---

# 8. 动作 Metadata

动作至少记录：

```text
name

start_frame
end_frame
total_frames
duration

has_bone_motion
has_morph_motion
has_camera
has_light
has_ik

is_camera_only
is_pose

thumbnail

tags[]

related_motion
related_camera

version_family
```

MMD 标准动作时间按照：

```text
30 FPS
```

计算。

例如：

```text
duration = total_frames / 30
```

---

# 9. Pose 判定

以下情况可以判定为 Pose：

### VPD

直接视为 Pose。

### VMD

如果：

- Bone / Morph 数据只存在于同一个时间点
- 没有连续时间动画
- 没有 Camera / Light 时间动画

则可以判定：

```text
is_pose = true
```

如果判断存在歧义，保留原始检测结果，并允许用户修正。

---

# 10. Motion / Camera 配套识别

MMD 动作包中经常存在：

```text
Dance.vmd
Dance_Camera.vmd
```

或者：

```text
Dance/
 ├─ Motion/
 │   └─ Dance.vmd
 │
 └─ Camera/
     └─ Dance_Camera.vmd
```

需要自动识别 Motion 与 Camera 的关系。

---

# 11. Camera 搜索范围

对于一个 Motion：

优先搜索：

```text
当前目录
```

同时允许搜索：

```text
当前动作所在目录向下最多两级子目录
```

即：

```text
Depth <= 2
```

用于发现 Camera。

---

# 12. 配套数据判定

综合以下信息评分：

```text
Normalized Filename
Common Prefix
Common Tokens
Edit Distance
Version Tokens
Directory Distance
Asset Type
```

需要自动去除常见描述词，例如：

```text
camera
cam
カメラ
motion
モーション
动作
动作数据
相机
camera_motion
```

再比较核心名称。

例如：

```text
TellYourWorld.vmd
TellYourWorld_camera.vmd
```

应具有非常高的关联评分。

---

# 13. Relation 必须保存置信度

不要只保存：

```text
A -> B
```

而应该保存：

```text
relation_type
source_asset
target_asset
confidence
reason
```

例如：

```text
MotionCameraPair
Confidence: 0.96

Reason:
normalized filename match
same parent package
camera suffix detected
```

低置信度 Relation 可以让用户确认。

---

# 14. 动作版本识别

动作数据可能存在：

```text
Dance.vmd
Dance_v2.vmd
Dance_fix.vmd
Dance_修正版.vmd
```

应该识别为：

```text
Version Family
```

但：

**不得自动删除、覆盖或合并。**

UI 中可以展示：

```text
Dance
 ├─ Original
 ├─ v2
 └─ fix
```

---

# 15. 动作 Thumbnail

用户可以在：

```text
Settings
```

指定：

```text
Motion Preview Model
```

所有 Motion Thumbnail 使用该模型进行生成。

加载：

```text
Preview Model
+
VMD / VPD
```

然后生成动作起始状态预览。

默认：

```text
Preview Frame = 0
```

如果 Frame 0 完全没有有效 Bone / Morph Key，则允许回退到：

```text
First Meaningful Keyframe
```

但需要在 Metadata 中记录实际使用的 Preview Frame。

---

# 16. Camera-only Thumbnail

如果 Asset 是纯 Camera 数据：

仍然使用统一 Preview Model。

Thumbnail 保持 Library 的统一视觉规范。

通过：

```text
Camera
```

Badge 明确标识该资产为 Camera。

不要为了 Camera Asset 改变整个资产库的 Thumbnail 风格。

---

# 17. Scene 资产

当前支持：

```text
PMX
PMD
X
```

作为场景资产。

至少读取：

```text
name
file_type
polygon_count
thumbnail
width
depth
area
tags
```

---

# 18. Scene 面积定义

场景面积统一定义为：

模型 Geometry Bounding Box 在：

```text
XZ Ground Plane
```

上的占地面积。

记录：

```text
width
depth
area
```

其中：

```text
area = width * depth
```

同时保留原始 MMD Coordinate Unit。

不要假设 MMD Unit 必然等于现实世界固定米制单位。

---

# 19. MMDRCV 资产卡片

定义新的资产卡格式：

```text
[AssetName].MMDRCV
```

MMDRCV：

```text
MMD Resource Card
```

该文件保存：

```text
Asset Metadata
+
Thumbnail
+
Source Fingerprint
```

---

# 20. MMDRCV 文件结构

MMDRCV 定义为：

> ZIP-compatible Container with Custom Extension

例如：

```text
Miku.MMDRCV
```

内部：

```text
/
├─ manifest.json
└─ preview.webp
```

禁止把：

```text
PMX
VMD
Texture
完整原始 Asset
```

复制到 MMDRCV 中。

MMDRCV 是 Card，不是资源打包格式。

---

# 21. manifest.json

至少包含：

```json
{
  "format": "MMDRCV",
  "schema_version": 1,

  "asset_id": "",
  "asset_type": "model",

  "name": "",

  "source": {
    "primary_file": "",
    "relative_path": "",
    "fingerprint": ""
  },

  "metadata": {},

  "tags": [],

  "thumbnail": {
    "file": "preview.webp",
    "width": 1024,
    "height": 1024,
    "format": "webp",
    "quality": 50
  },

  "created_at": "",
  "updated_at": "",

  "generator": {
    "name": "MMDbridgeLib",
    "version": ""
  }
}
```

字段必须支持向后兼容扩展。

---

# 22. MMDRCV 位置

MMDRCV 必须存放在对应资产附近。

例如模型：

```text
Miku/
 ├─ Miku.pmx
 ├─ Miku.MMDRCV
 └─ texture/
```

动作：

```text
Motion/
 ├─ Dance.vmd
 └─ Dance.MMDRCV
```

场景：

```text
Stage/
 ├─ Stage.pmx
 └─ Stage.MMDRCV
```

---

# 23. Card 文件名

默认：

```text
[AssetName].MMDRCV
```

但 Windows 文件系统可能无法保存某些字符。

因此需要：

```text
Display Name
```

和：

```text
Safe Filename
```

分离。

如果发生：

- 非法字符
- 文件名冲突
- 重复 AssetName

允许生成：

```text
AssetName__ShortID.MMDRCV
```

不得覆盖其他 Card。

---

# 24. Card Source Fingerprint

MMDRCV 必须记录 Primary Source Fingerprint。

至少考虑：

```text
file_size
mtime
content_hash
```

扫描时区分：

```text
CardMissing
CardValid
CardStale
CardBroken
```

如果 PMX / VMD 等资源变化：

旧 Card 不应继续被无条件认为有效。

---

# 25. Thumbnail 基本规格

所有自动生成 Thumbnail 使用统一标准：

```text
1024 × 1024
WebP
Quality = 50
1:1
```

目标：

```text
高读取性能
足够清晰
Library 卡片视觉统一
磁盘占用合理
```

---

# 26. Thumbnail 视角

模型 / 动作 / 场景自动 Thumbnail 应尽量采用：

```text
Orthographic
Front View
```

视觉参考：

```text
PMXEditor 默认打开模型时的预览效果
```

要求：

- 正面
- 正交
- 自动居中
- 自动缩放
- 完整显示主体
- 保留合理边距
- 不裁掉头、脚或场景主体
- 不因模型尺寸不同导致视觉尺度严重失控

正面坐标系方向必须通过测试 Asset 与 PMXEditor 对照确定。

不要凭开发者主观推断坐标方向。

一旦确定，通过 Golden Test 固化。

---

# 27. Thumbnail Renderer

Thumbnail Renderer 必须属于 MMDbridgeLib 本体。

Agent 不负责自己截图。

正确流程：

```text
Agent
 ↓
MMDbridge API
 ↓
Thumbnail Renderer
 ↓
preview.webp
```

Renderer 应支持：

```text
PMX
PMD
X
VMD
VPD
```

至少满足当前 Card 需求。

---

# 28. Thumbnail Render Queue

必须实现真正的 Job Queue。

支持：

```text
enqueue
cancel
retry
priority
progress
status
batch
```

状态：

```text
Pending
Parsing
Rendering
Encoding
Completed
Failed
Cancelled
```

支持批量任务。

---

# 29. 并发

Thumbnail 系统需要支持并发。

但不要无上限并发占用 GPU / CPU。

分别限制：

```text
Parse Concurrency
Render Concurrency
Encode Concurrency
```

允许：

```text
Auto
1
2
4
8
Custom
```

例如：

```text
CPU Parsing × 8
GPU Rendering × 2
WebP Encoding × 4
```

由 Job Scheduler 控制。

---

# 30. Thumbnail Cache

相同：

```text
Asset Fingerprint
+
Renderer Version
+
Preview Settings
```

生成的 Thumbnail 应支持缓存。

避免每次扫描都重新渲染。

---

# 31. Database

使用 SQLite 保存资产 Library Index。

数据库保存：

```text
roots
assets
asset_files
metadata
tags
asset_tags
relations
duplicates
versions
cards
jobs
scan_state
settings
```

SQLite 是：

```text
Index / Cache / Library State
```

不是 Asset 的唯一真实来源。

真实来源仍然是：

```text
Filesystem
+
MMDRCV
```

理论上删除 SQLite 后，重新 Scan 应能够重建 Library。

---

# 32. 搜索

至少支持：

```text
Asset Name
Filename
Path
Tag
```

搜索必须快速。

建议支持：

```text
Fuzzy Search
Token Search
Japanese / Chinese Unicode Search
```

---

# 33. Filter

Filter 应尽可能丰富，并允许组合。

基础：

```text
Asset Type
Root
Directory
Tag
Favorite
Card Status
Duplicate Status
Relation Status
Recently Added
Recently Modified
Needs Review
```

Model：

```text
Polygon Count
Bone Count
Has Thumbnail
Has Card
```

Motion：

```text
Frame Count
Duration
Has Bone Motion
Has Morph
Has Camera
Camera Only
Pose
Has Paired Camera
Version Family
```

Scene：

```text
File Type
Polygon Count
Width
Depth
Area
```

Filter 支持：

```text
AND
OR
NOT
```

并支持：

```text
Saved Filter
```

形成 Smart Collection。

---

# 34. Tag

Tag 是非常重要的一级数据。

支持：

```text
用户 Tag
Agent Tag
自动解析 Tag
```

Tag 应记录来源。

例如：

```text
tag = "初音ミク"
source = agent
```

或者：

```text
tag = "Camera"
source = parser
```

用户手动修改结果优先级高于自动 Tag。

Agent 不得在下一次扫描时擅自覆盖用户修改。

---

# 35. Duplicate Detection

实现资产去重检测。

第一阶段至少支持：

### Exact Duplicate

根据：

```text
Content Hash
```

识别。

### Possible Duplicate

综合：

```text
Name
File Size
Polygon Count
Bone Count
Duration
Frame Count
Fingerprint
Directory Structure
```

给出：

```text
Similarity Score
```

---

# 36. Duplicate 安全原则

Duplicate Detection 只负责：

```text
发现
展示
建议
关联
```

不得：

```text
自动删除
自动覆盖
自动合并
```

除非用户明确执行操作。

---

# 37. Asset Manager

软件不是只读 Viewer。

需要具备 Asset Manager 功能。

至少规划：

```text
Rename
Move
Delete
Tag
Favorite
Reveal in Explorer
Open Source Directory
Batch Operation
```

---

# 38. 文件操作安全

任何：

```text
Move
Rename
Delete
```

操作都必须先：

```text
Resolve Asset Files
Resolve Dependencies
Generate Operation Plan
```

然后再执行。

禁止仅移动 PMX 而遗留：

```text
Texture
Sphere
Toon
Readme
Related Files
```

导致模型损坏。

优先以：

```text
Asset Package / Directory
```

作为移动单位。

---

# 39. Delete

删除资产默认使用：

```text
OS Recycle Bin
```

而不是永久删除。

永久删除必须：

```text
Explicit Action
+
Confirmation
```

---

# 40. Operation Journal

建议维护文件操作 Journal：

```text
operation
source
destination
timestamp
result
```

为后续 Undo / Recovery 留出基础。

---

# 41. UI

主要设计语言：

> Steam Library

不是 Windows Explorer。

首页应以资产内容本身为视觉中心。

---

# 42. Library Layout

建议：

```text
┌─────────────────────────────────────┐
│ Search                  Filter      │
├─────────┬─────────────────┬─────────┤
│ Library │                 │ Details │
│         │     Cards       │         │
│ Models  │                 │         │
│ Motion  │                 │         │
│ Scene   │                 │         │
│         │                 │         │
│ Tags    │                 │         │
│ Smart   │                 │         │
├─────────┴─────────────────┴─────────┤
│ Background Jobs                    │
└─────────────────────────────────────┘
```

---

# 43. Card

Card 以 Thumbnail 为主体。

至少显示：

```text
Thumbnail

Name

Primary Tags

Asset Type Badge
```

不同类型使用明显 Badge：

```text
MODEL
MOTION
CAMERA
POSE
SCENE
```

不要在 Card 上堆满 Metadata。

详细数据放 Inspector。

---

# 44. 大型 Library 性能

假设用户可能拥有：

```text
10,000+
50,000+
```

资产。

因此：

- Card Grid 必须 Virtualized
- Thumbnail Lazy Load
- 后台扫描不能卡住 UI
- 扫描不能阻塞渲染
- Hash 不应每次全库重新计算
- Metadata Parser 应支持增量缓存
- 数据库查询需要 Index

---

# 45. Core API

MMDbridge Core 必须提供稳定的结构化 API。

UI 不应该直接操作 SQLite。

Agent 更不应该直接操作 SQLite。

API 返回：

```text
Versioned JSON
```

---

# 46. CLI

为了让：

```text
Codex
Claude Code
其他 Agent
自动化脚本
```

能够使用 MMDbridgeLib，需要提供 CLI。

例如：

```text
mmdbridge roots list --json

mmdbridge scan --root <id> --json

mmdbridge assets list --type model --json

mmdbridge assets inspect <asset-id> --json

mmdbridge cards pending --json

mmdbridge thumbnail enqueue <asset-id> --json

mmdbridge card create <asset-id> --json

mmdbridge card refresh <asset-id> --json

mmdbridge card verify <asset-id> --json

mmdbridge jobs list --json
```

CLI 不应该重新实现逻辑。

它只调用：

```text
MMDbridge Core
```

---

# 47. Agent Skill

创建一个通用 Agent Skill，例如：

```text
skills/
└─ mmdbridge-card-manager/
   └─ SKILL.md
```

Skill 面向：

```text
Codex
ChatGPT Agent
Claude Code
其他能够执行 CLI 的 Agent
```

尽可能避免依赖某一个特定 Agent 的私有能力。

---

# 48. Skill 目标

Skill 的主要任务：

> 从 MMDbridgeLib 已配置的 Asset Root 中寻找尚未创建有效 MMDRCV Card 的资产，并调用 MMDbridgeLib API 完成解析、判断、Thumbnail 生成和 Card 创建。

---

# 49. Skill 不直接扫描未知路径

正常模式下：

Agent 首先通过：

```text
mmdbridge roots list
```

获取用户已经配置的目录。

再通过：

```text
mmdbridge cards pending
```

获取：

```text
CardMissing
CardStale
CardBroken
```

资产。

Agent 不应自己重新维护一套 Asset Database。

---

# 50. Skill Workflow

标准流程：

```text
1. Query Library

2. Find Assets Without Valid Card

3. Inspect Asset Candidate

4. Read:
   metadata
   directory structure
   filenames
   parser result
   relations

5. Determine:
   asset name
   asset type
   tags
   variant/version relation

6. Request Thumbnail

7. Wait / Poll Job

8. Create MMDRCV

9. Verify MMDRCV

10. Update Library Index
```

---

# 51. Agent Tagging

Agent 可以根据：

```text
文件名
目录名
PMX Internal Name
相关文件
README
相邻资产
Motion / Camera 配套关系
```

综合生成 Tag。

但要求：

- 不确定的信息不要装作确定
- Tag 可以带 Confidence
- 低置信度结果可以标记 NeedsReview
- 不覆盖用户手工 Tag
- 不凭空生成作者、角色等事实

---

# 52. Skill Batch

支持：

```text
scan one asset
scan directory
scan root
scan all pending
```

例如：

```text
Generate cards for 20 pending models
```

Agent 可以批量请求 Thumbnail。

Thumbnail Queue 负责实际并发。

Agent 自己不要同时启动几十个 Renderer Process。

---

# 53. Skill Safety

Skill 默认不得：

```text
Move Asset
Rename Asset
Delete Asset
Merge Asset
Delete Duplicate
```

Card Generation 模式只允许：

```text
Read
Analyze
Thumbnail
Create / Update MMDRCV
Update Index
```

任何 Asset Manager 写操作必须由用户明确要求。

---

# 54. MMDRCV 应由 API 创建

Agent 不应手工：

```text
zip manifest.json preview.webp
```

MMDbridgeLib 必须提供：

```text
Card Writer
```

负责：

```text
Schema Validation
Filename
Fingerprint
Thumbnail
Atomic Write
```

这样可以避免不同 Agent 创建出不兼容 Card。

---

# 55. Atomic Write

创建：

```text
Asset.MMDRCV
```

时：

先生成：

```text
Asset.MMDRCV.tmp
```

验证成功后：

```text
Atomic Rename
```

避免程序崩溃留下损坏 Card。

---

# 56. Parser 与 Renderer 解耦

保持：

```text
Parser
Renderer
Card Writer
Scanner
```

彼此解耦。

例如：

```text
PMX Parser
   ↓
Asset Metadata

Asset Metadata
   ↓
Thumbnail Renderer

Metadata + Thumbnail
   ↓
Card Writer
```

后续才方便支持：

```text
PMD
VRM
FBX
其他资产
```

而不需要重写整个 Library。

---

# 57. Renderer 建议

如果技术可行，Thumbnail Renderer 优先考虑：

```text
Rust
+
wgpu
```

实现 Offscreen Render。

不要依赖用户实际打开一个 WebView 页面然后截图。

Renderer 应可以在：

```text
Headless / Worker
```

状态下工作。

具体渲染实现允许根据技术验证调整，但必须保持统一 Core API。

---

# 58. Error Handling

任何错误必须输出：

```text
error_code
message
asset_id
source
recoverable
```

例如：

```text
MissingTexture
InvalidPMX
UnsupportedX
CardWriteDenied
ThumbnailRenderFailed
HashFailed
```

不要仅返回：

```text
failed
```

---

# 59. Scan 状态

Asset 支持：

```text
Ready
NeedsReview
MissingSource
ParseFailed
Unsupported
CardMissing
CardStale
CardBroken
```

同一 Asset 可以拥有多个状态标签。

---

# 60. 开发阶段

不要一开始同时实现所有功能。

按照下面顺序推进。

## Phase 0 — Specification

首先建立：

```text
docs/ARCHITECTURE.md

docs/ASSET_DISCOVERY.md

docs/MMDRCV_SPEC.md

docs/THUMBNAIL_RENDERER.md

docs/MOTION_RELATION.md

skills/mmdbridge-card-manager/SKILL.md
```

先固定数据结构和边界。

---

## Phase 1 — Core Library

实现：

```text
SQLite

Asset Root

Scanner

Model Detection

PMX Parser

VMD Parser

Scene Parser

Asset Index
```

先确保：

```text
扫描
解析
数据库
```

可靠。

---

## Phase 2 — Library UI

实现：

```text
Steam-style Library

Model / Motion / Scene

Search

Filter

Tag

Inspector
```

---

## Phase 3 — Thumbnail

实现：

```text
PMX Preview

Motion Preview

Scene Preview

1024 WebP

Render Queue

Cache

Concurrency
```

---

## Phase 4 — MMDRCV

实现：

```text
MMDRCV Reader

MMDRCV Writer

Card Validation

Card Stale Detection
```

---

## Phase 5 — Agent API

实现：

```text
CLI

JSON API

Pending Card Query

Thumbnail API

Card API
```

然后完善：

```text
SKILL.md
```

---

## Phase 6 — Asset Intelligence

实现：

```text
Motion / Camera Pair

Version Family

Duplicate Detection

NeedsReview
```

---

## Phase 7 — Asset Manager

最后再加入：

```text
Move

Rename

Delete

Batch Operations

Operation Journal
```

避免在 Asset Discovery 尚未稳定时引入危险写操作。

---

# 61. 首轮验收

至少准备真实 MMD 测试 Asset，包括：

```text
正常 PMX 模型

散落 PMX

同目录多个 PMX

PMX 不同版本

正常 VMD

Motion + Camera

Camera 在一级子目录

Camera 在二级子目录

多个 Motion Version

VPD Pose

Camera-only VMD

PMX Scene

PMD Scene

X Scene

缺失贴图模型

损坏 PMX

中文路径

日文路径

超长路径
```

---

# 62. Model 验收标准

软件能够：

1. 添加多个 Model Root
2. 正确扫描 PMX
3. 识别正常目录模型
4. 对异常散落目录生成 Candidate
5. 获取名称
6. 获取面数
7. 获取骨骼数
8. 生成统一 Thumbnail
9. 创建 MMDRCV
10. 重新扫描后读取 Card

---

# 63. Motion 验收标准

软件能够：

1. 读取 VMD
2. 获取总帧数
3. 计算时间长度
4. 判断 Bone Motion
5. 判断 Morph
6. 判断 Camera
7. 判断 Pose
8. 使用指定 Preview Model
9. 生成起始动作 Thumbnail
10. 自动发现同目录 Camera
11. 自动发现两级目录以内 Camera
12. 对名称近似 Motion / Camera 建立 Relation
13. 识别不同 Version

---

# 64. Scene 验收标准

软件能够：

1. 识别 PMX
2. 识别 PMD
3. 识别 X
4. 读取面数
5. 计算 Bounding Box
6. 计算 XZ Width
7. 计算 XZ Depth
8. 计算 Area
9. 生成统一 Thumbnail
10. 创建 MMDRCV

---

# 65. Thumbnail 验收标准

同一个 Asset：

只要：

```text
Asset
Renderer Version
Preview Settings
```

没有变化：

Thumbnail 应保持确定性。

不同机器允许因为 GPU 精度产生极小差异，但：

```text
Camera
Projection
Framing
Pose
Lighting
Background
```

必须一致。

---

# 66. 软件目标

最终 MMDbridgeLib 应做到：

```text
用户只需要把大量 MMD 文件夹加入 Library。

MMDbridgeLib 自动扫描资产。

软件识别：

什么是模型
什么是动作
什么是场景

然后：

读取 Metadata
建立索引
建立关系
发现版本
发现重复
生成 Thumbnail

尚未整理的资产：

交给 Agent Skill

Agent 调用 MMDbridgeLib 自身提供的能力：

分析
生成标签
生成 Thumbnail
创建 MMDRCV

最终所有资产形成一个类似 Steam Library 的：

可视化
可搜索
可筛选
可管理
可扩展

的 MMD Asset Library。
```

---

# 67. 开发原则

始终优先：

```text
数据正确性
>
资产安全
>
可恢复性
>
性能
>
UI 动效
```

不允许为了 UI 快速完成而复制 Asset Logic 到前端。

不允许为了 Agent 自动化而绕过 MMDbridge Core。

不允许因为自动分类方便而擅自移动或删除用户文件。

不确定的 Asset Relation：

```text
记录 Confidence
+
NeedsReview
```

而不是假装判断正确。

整个项目的核心应该始终保持：

> 一个统一的 MMD Asset Core，被 UI、CLI 和 Agent Skill 共同调用。
