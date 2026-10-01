# MMDbridgeLib alpha0.1

首个公开 Alpha 预览版本。Git 标签为 `alpha0.1`，程序与包版本为 `0.1.0-alpha.1`。

## 本次发布

- 本地 MMD 资产库：递归扫描、缩略图卡片、目录树、搜索与组合筛选。
- 用户标签、收藏、智能集合及批量标签 / 收藏操作。
- PMX 模型 3D 预览、权重类型统计、逐骨骼权重与 SDEF 参数检查。
- PMX / PMD 场景预览，VMD 动作播放与配套镜头，VPD 姿势缩略图。
- 动作 / 相机与版本家族关联建议，纯相机资源作为动作辅助内容保留。
- 扫描进度与队列控制、可取消和重试的缩略图任务、显式批量重新生成缩略图。
- 资产包移动、重命名、回收站删除及本地操作日志。
- 当前版本包含新的桌面布局、模型透明材质显示调整和场景视角控制。
- 完善中文 README，并统一桌面程序、CLI、Core 与依赖锁文件中的版本号。

## 下载与使用

下载 `MMDbridgeLib-alpha0.1-windows-x64-portable.zip`，解压到可写目录后运行 `mmdbridge-desktop.exe`。包内的 `mmdbridge.exe` 为命令行工具，普通使用可直接打开桌面程序。

适用于 Windows 10 / 11 x64；桌面界面需要 Microsoft Edge WebView2 Runtime。发布的是便携版，不包含安装器、示例素材或个人资产库。

两个 EXE 共用所在目录下的 `data/library.sqlite3`。升级已有便携版时，先退出程序并备份 `data` 目录，再替换 EXE，保留原数据目录。缩略图资源卡可能保存在素材旁的 `.MMDRCV` 文件中。

在资产库中添加模型、动作或场景目录即可建立索引。VMD / VPD 预览需在设置中选择一个 PMX 预览模型。

## 当前限制

- 项目仍在施工中，功能、界面和数据结构可能调整。
- 模型索引及模型权重查看以 PMX 为范围；场景支持 PMX / PMD，动作与姿势支持 VMD / VPD。`.X` 已退出支持。
- VPD 当前通过缩略图查看；复杂物理、透明材质与变形效果仍需持续完善，预览可能与 MMD 有差异。
- 此次发布检查涵盖前端构建、桌面与 CLI Release 构建、CLI 版本和便携包内容。另以无窗口 Edge 截取当前前端预览，使用匿名化只读素材数据、已有 Core 模型数据和只读接口替身；未启动真实桌面 UI，也未进行全库扫描或完整素材兼容性回归。

## 界面预览

素材名称、路径、标签和缩略图已做隐私处理。便携包包含全部四张预览图，更多界面请参见 [README](https://github.com/JDui/MMDbridgeLib/tree/alpha0.1#界面预览)。

![资产库与详情](https://raw.githubusercontent.com/JDui/MMDbridgeLib/alpha0.1/docs/images/alpha0.1/01-library.png)

![模型权重查看器](https://raw.githubusercontent.com/JDui/MMDbridgeLib/alpha0.1/docs/images/alpha0.1/03-model-weights.png)

欢迎在 [Issues](https://github.com/JDui/MMDbridgeLib/issues) 中提供复现步骤和问题截图；分享素材时请遵守作者的配布规则。
