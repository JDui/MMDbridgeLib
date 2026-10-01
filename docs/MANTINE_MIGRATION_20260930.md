# Mantine 界面迁移

2026-10-01 已追加打包 CSP、真实浏览器离屏布局与交互后的完整复核，见 [UI_LAYOUT_AUDIT_20261001.md](UI_LAYOUT_AUDIT_20261001.md)。以下记录是初始迁移阶段的验证范围。

用户明确要求“转向 Mantine”，本次范围覆盖现有桌面前端的通用 UI；沿用 Tauri、Rust Core、资产虚拟列表与现有 Three.js 渲染。

## 实现

- 引入固定版本 `@mantine/core`、`@mantine/hooks` 9.6.3；React/ReactDOM 声明调整为当前已安装的 19.3.0 系列，以满足 Mantine 的 React >=19.2 对等依赖。安装没有更新既有 React 实际版本。
- `main.tsx` 设置 MantineProvider、官方样式与强制深色主题。`theme.ts` 统一薄荷色、深色、字体、紧凑尺寸、控件与弹窗默认配置。
- Library 用 AppShell 管理左侧导航、主区域和右侧检查器；资产类型与动作格式用 Tabs。保留独立滚动的目录树、VirtuosoGrid、分页、选择、收藏、缩略图缓存、后台任务和 Core IPC。
- 按钮、图标按钮、输入、选择、复选框、滑块、类型徽章、扫描进度和提示改用 Mantine。自定义资产卡片、目录卡片、导航和权重热点列表沿用布局，通过 UnstyledButton 保留复杂子元素。
- 标签筛选采用可搜索 MultiSelect，可多选、删除、清空；保留最多 24 个标签及 AND/OR 组合。下拉最多显示 80 项匹配结果，避免直接创建整个标签库的选项 DOM。
- 设置、添加目录、缩略图、扫描队列、操作计划和操作日志使用 Modal。LibraryDialog 将原有同步浏览器 prompt/confirm 改为异步 Mantine 弹窗，取消返回 null/false，不进入后续资产操作。
- 资产与目录右键菜单使用 Menu，沿用原有操作处理器；由组件处理浮层定位、外部点击、Escape 和箭头导航。应用仅维护互斥菜单状态和窗口大小变化时的关闭。
- PMX/场景/VMD 查看器使用 Modal.Root 和 Mantine 控件，保留渲染算法、飞行/轨道控制、时间轴行为和资源释放。查看器使用 `withinPortal={false}`，确保现有 mount effect 执行前画布宿主已挂载；移除重复的自定义 Escape 监听。
- 删除旧控件、右键菜单和遮罩的样式规则，保留产品布局和资产图形样式；`mantine-layout.css` 仅处理布局、滚动、组件外壳和宿主尺寸。
- Library 页面延迟加载，保留启动状态轮询、真实阶段/数量、耗时与错误信息；Loader/Progress/Alert 提供启动反馈。

## 验证与边界

- `npm run build`：TypeScript 和 Vite 生产构建通过。入口约 392 KB，Library 延迟块约 229 KB；3D 查看器继续延迟加载。ModelViewer 的 513 KB 包体仍触发 >500 KB 提示，不是构建错误，也不是启动耗时测量。
- `npm ls @mantine/core @mantine/hooks react react-dom --depth=0`：9.6.3/9.6.3/19.3.0/19.3.0，无对等依赖错误。
- 使用独立忽略目录 `target/mantine-ui-probe` 的 jsdom 26、React act 与 Tauri 官方 mockIPC，未把 jsdom 加入产品依赖。`node target/mantine-ui-probe/run.cjs` 通过：
  - 实际 App 挂载 AppShell 导航/检查器，默认请求 Library 全部资产。
  - 实际 MultiSelect 选择标签和 NativeSelect 选择非标准骨架，IPC 条件正确；清空后表达式为 null。
  - 设置 Modal 打开，修改解析并发为 4，保存参数正确。
  - 添加目录取消、目录菜单重命名取消不提交对应 IPC。
  - 异步确认取消为 false，确认返回 true。
  - 实际菜单箭头键进入首项，Escape 调用关闭。
  - 与 3D 查看器同配置的 Modal.Root 在初次 effect 时已挂载宿主元素。
- 源码检索：桌面 src 中不再使用原生 button/input/select JSX、浏览器 prompt/confirm 或自行实现的 dialog 遮罩。
- `git diff --check` 通过。
- 无窗口交互使用模拟 IPC 和 jsdom，不验证真实数据库、WebView2 布局、GPU 3D 渲染、焦点与滚动的实机视觉效果。不启动或操作桌面程序/用户前台窗口。
- 本次没有修改 Rust 资产逻辑、写入真实素材、提交或推送。已有启动/缩略图修复的未提交改动保留。

## 交付

从 `apps/desktop` 执行离线便携构建 `npm run tauri -- build --no-bundle`，成功后复制至 `E:\MMD\MBL\mmdbridge-desktop.exe`；不生成安装器。CLI 逻辑本次未变，沿用先前已部署版本。
