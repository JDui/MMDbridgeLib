# PMD / PMX 加载与缩略图迭代记录（2026-10-06）

本次沿用 Rust Core、`mmd-anim-format 0.5.2`、`mmd-anim-runtime 0.5.2` 和 WGPU，未增加生产依赖或第二套模型解析器。缩略图渲染器更新为 **0.6.0**。现有玻璃界面保留操作、状态与必要说明，没有添加标语。

## 已完成的修复

| 问题 | 当前行为 |
| --- | --- |
| PMD 可作为场景读取，但模型分类、打开对话框和查看器入口不一致 | Model 根目录、直接打开、查看器、缩略图与动作预览设置均接受 PMD / PMX |
| PMD 顶点权重和骨骼未进入模型查看器 | 复用 MMDV v3，传输 PMD 两骨骼权重、骨骼名称和位置 |
| 异常计数可能让解析器提前分配大数组 | 读取与解码前检查字节上限、区段数量及最小记录长度；PMD 可选英文、Toon、物理尾段仍按已有编解码器规则读取 |
| 非三角索引或顶点索引越界可能进入查看器 / GPU | Core 与查看器检查三角索引及顶点范围；GPU 上传检查设备缓冲区上限 |
| Sphere 名称在 PMD 贴图对的第一位时选错 diffuse | 按 `.sph` / `.spa` 扩展识别 Sphere，选择非 Sphere 的 diffuse |
| PMD Toon 255 被误当成共享 Toon | 255 禁用 Toon；0–9 对应标准 Toon 1–10 |
| PMX Sphere SubTexture 误用普通 UV | 使用第一附加 UV 的 XY，缺少该通道时记录诊断并禁用该模式；对应 UV Morph 同步更新 |
| 小骨架边界裁掉头发、裙摆和配件 | 缩略图按实际被三角形引用的网格顶点取景，增加 8% 留白；忽略未使用的远处顶点 |
| 查看器初始镜头过近 / 窄视口裁切 | 模型与动作查看器按水平、垂直视角中较窄者计算带留白的镜头距离；回归包含五种宽高比 |
| 无 WebGL 时动作查看器抛出未处理异常 | 初始化失败在查看器内显示错误，禁用播放并允许关闭返回资产库 |
| 双击第二排卡片可能打开错误资产 | 单击选择不再自动展开详情栏 / 重排网格，详情按钮控制开合；已打开的详情栏继续随选择更新 |
| GPU 初始化失败被永久保留 | 只缓存成功的渲染器实例，后续任务可重新初始化 |
| 加载大批材质会持续占用内存 | 单次渲染外部贴图预算 256 MiB，按剩余预算降低尺寸或记录回退；材质间检查取消与进度 |
| Windows 分隔符贴图在其他平台找不到，大小写不同路径误合并 | 原始路径优先，再尝试规范化分隔符；只有 Windows 才忽略缓存键大小写 |
| 动作缩略图沿用预览模型的旧贴图 | 缓存版本加入实际引用 diffuse / sphere / toon 的大小与修改时间，以及缺失到存在的变化 |
| 损坏骨架覆盖可用的动作预览设置 | 保存前验证 runtime，失败保留原设置和卡状态；静态 PMX 允许带诊断的 rest-mesh 回退 |

GPU 使用设备支持的 4× MSAA，否则回退至单采样，报告记录 `antialiasingSamples`。原有 1024×1024 WebP、质量 50、阶段并发限制、目标池及资源卡版本校验继续使用。旧版缩略图会按渲染器 / 设置版本标记待更新，设置中的“重新生成全部缩略图”使用已有可取消、可重试的后台队列。GPU 等待和读回均有超时；不能在已经提交的单次 GPU draw 中途取消。

## 支持范围与边界

| 功能 | 当前范围 | 本轮验证与限制 |
| --- | --- | --- |
| PMX 2.0 / 2.1 | UTF-8 / UTF-16、1 / 2 / 4 字节索引；BDEF1 / 2 / 4、SDEF；现有 QDEF 保留 | 合成矩阵 54 组：2.0 排除 QDEF，2.1 包含 QDEF；不代表真实模型全集通过 |
| PMD | Shift-JIS 元数据、两骨骼权重、可选英文 / Toon / 物理尾段解析、Model / Scene 分类 | 大写扩展与中日文临时路径进入集成测试；物理段解析不等于物理模拟 |
| PMD 动作静帧 | VMD / VPD 骨骼姿态，已有 runtime IK，VMD 顶点 Morph 通过 base morph 表定位 | 生成 PMD + VMD / VPD 离屏渲染；新增 base-index 数值测试。PMD 物理未模拟，复杂 IK 未与 MMD 金图对照；现有导入器仅建立基础父子骨架，不宣称所有 PMD bone type 的附加行为等价 |
| 缩略图材质 | diffuse、ambient、specular、Toon、multiply / add Sphere；PMX SubTexture UV1；alpha cutout / blend | 离屏样例涵盖普通贴图、UV1、SDEF、透明裁切、PMD Sphere。复杂交叉透明面仍可能有排序差异 |
| PMX Morph / 相机 | 沿用顶点、group、bone、UV1、material、flip、impulse 静帧，以及 VMD 相机轨道 | 本轮没有重新验证真实复杂 Morph / 相机组合；impulse 为既有有界静帧近似 |
| 实时动作查看器 | PMX / PMD runtime 骨骼矩阵和 VMD 相机 | PMD 骨骼帧进入探针；SDEF / QDEF 实时近似线性，实时顶点 / 材质 Morph 尚未支持 |
| 静态不完整骨架 | 网格有效但 PMX runtime 构建失败时显示 rest mesh，并记录回退原因 | 不允许把该模型设置为动作预览 rig；结构损坏、无效网格仍报错 |

内置模型 / 动作查看器显示 diffuse 贴图与材质颜色；上述 Toon / Sphere 的完整材质路径在 Core 缩略图中验证。输入贴图格式沿用 BMP / JPEG / PNG / TGA，其余格式保留加载诊断与回退。

受控资源上限为：源文件 512 MiB、文本字段 16 MiB、顶点 1,000,000、三角索引 12,000,000、材质 8,192、骨骼 16,384；额外 UV 最多 4 通道。超出资源上限的合法模型也会明确拒绝。256 MiB 是外部贴图预算，不是整个进程或所有并发任务的内存上限。贴图大小 / 修改时间缓存不是内容安全签名。

## 测试与截图证据

`tests/fixtures/mod.rs` 生成模型二进制；`tests/model_loading.rs` 通过公开 Library API 检查扫描、格式支持、查看器载荷、逐字节截断、异常计数、贴图引用和设置失败后的保留行为。缩略图单元测试覆盖 Sphere 顺序、Toon 255、UV1、静态回退、贴图预算、分隔符、缓存变化及 PMD Morph 基础顶点映射。

`examples/compatibility_probe.rs` 生成五个 4,096 三角形模型和两份 PMD 动作文件，调用实际 Core 离屏渲染，再执行资源卡创建、校验和缩略图保留流程。生成的贴图与模型仅存在于新的临时测试目录，没有读取、修改、搬移或删除用户素材。

`scripts/model-review.mjs` 使用当前前端和 Core 生成的 WebP / MMDV 网格，隔离无窗口浏览器检查七张图像解码、PMD 查看器载入、权重显示、模型加载失败后的关闭恢复，以及模拟无 WebGL 时动作查看器的关闭恢复。该流程同时检查单击不改变详情栏开合，以及双击打开的资产名称正确。界面的 Tauri 桥为测试替身；截图属于 **Linux CI 合成样例**，不是 Windows 实机截图，也不是既有用户模型的渲染证明。

验证源码：`dbb206f79d55c30a70d575d5ca75eb103983ec85`。

| 检查 | 结果 / 证据 |
| --- | --- |
| Core 单元测试 | 42 通过；2 项已有真实模型 / 数据库副本探针未运行 |
| 模型加载集成测试 | 4 通过，其中 PMX 格式矩阵为 54 组合 |
| CLI 回归 | 4 通过 |
| 前端功能测试 / 构建 | 13 通过；TypeScript / Vite 构建通过 |
| Windows / Linux 回归 | [Functional regression 37401232916](https://github.com/JDui/MMDbridgeLib/actions/runs/37401232916) 全部通过；Windows `cargo check` 桌面端通过 |
| 实际离屏渲染与查看器恢复 | [Model compatibility 37401232870](https://github.com/JDui/MMDbridgeLib/actions/runs/37401232870) 通过；7 张 1024×1024 WebP，五个模型各 2,145 顶点 / 4,096 三角形 |
| GPU 证据 | `llvmpipe (LLVM 20.1.2, 256 bits) (Vulkan/Cpu)`，实际采样数 4；这是软件 Vulkan |
| 隔离 UI 回归 | 七图解码、PMD 权重查看、加载错误关闭、无 WebGL 错误关闭、双击目标稳定；`pageErrors=[]` |
| 操作日志界面回归 | [Isolated visual review 37401232825](https://github.com/JDui/MMDbridgeLib/actions/runs/37401232825) 通过 |
| Windows 便携版 | [Portable Windows build 37401232953](https://github.com/JDui/MMDbridgeLib/actions/runs/37401232953) 通过；`npm run tauri -- build --no-bundle` 生成 Windows x64 `MMDbridgeLib.exe`，24,268,800 字节；未启动程序 |

阶段截图均为 1480×960 JPEG：

1. `MBL_Model_Stage1_Library_Dark.jpg`：深色资产库与七张实际 Core 缩略图。
2. `MBL_Model_Stage2_PMD_Weights.jpg`：PMD BDEF2 权重统计与完整初始取景。
3. `MBL_Model_Stage3_Library_Light.jpg`：浅色资产库。
4. `MBL_Model_Stage4_Load_Error.jpg`：模拟模型数据截断的错误提示，关闭后恢复资产库。
5. `MBL_Model_Stage5_No_GPU.jpg`：模拟 WebGL 上下文不可用，动作查看器显示可关闭错误并禁用播放。

截断 PMD / 异常计数在 Core 集成测试中实际验证；第 4 张 UI 截图使用桥接层模拟同类错误。第 5 张在浏览器中将 WebGL 上下文创建返回值设为 null，验证真实初始化异常路径。前端最后一轮回归发现并修复了“首次点击展开详情导致第二击落到另一资产”的重排问题。

## 技术依据

- [现有 PMD 编解码与 runtime 源码，固定版本 0.5.2](https://docs.rs/mmd-anim-format/0.5.2/src/mmd_anim_format/pmd.rs.html)：复用其可选尾段和 runtime 导入，保留材质 / 物理 runtime 尚未等价的边界。
- [现有 PMX 编解码源码，固定版本 0.5.2](https://docs.rs/mmd-anim-format/0.5.2/src/mmd_anim_format/pmx/mod.rs.html)：读取前校验围绕其既有布局和长度规则，避免复制完整解析器。
- [PmxEditor 0.2.3.6 附带规格备份](https://gist.github.com/FlandreDaisuki/90ae5abf3138a15994526b6bfec73c2c)：SubTexture 使用附加 UV1，共享 Toon 与索引规则。
- [WGPU 30.0.1 TextureFormatFeatureFlags](https://docs.rs/wgpu/30.0.1/wgpu/struct.TextureFormatFeatureFlags.html)：按颜色 resolve 和深度格式能力选择采样数。

## 尚未验证

本环境没有用户 Windows 素材目录、MMD / PMXEditor 金图或实际桌面 GPU。真实模型大样本、特殊 PMD 导出器尾段、极大模型、复杂透明 / IK / Morph 的视觉等价，以及 Windows 字体、动效和 GPU 交互仍需独立验证。本轮结果不应被表述为“所有 PMD / PMX 均可加载”。

便携版只构建 EXE，不构建安装器或启动桌面窗口。用户目标目录为 `E:\MMD\MBL`；本环境不能直接写入该磁盘。现有数据库位置仍是 EXE 同目录下 `data\library.sqlite3`。

交付归档：`MMDbridgeLib_Portable_20261006.zip`（仅含 EXE），`MBL_Model_Screenshots_20261006.zip`（五张截图、七张 Core WebP 与机器验证记录）。归档和报告对应上列验证源码，不包含安装器、数据库或用户素材。
