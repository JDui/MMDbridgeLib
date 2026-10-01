# 桌面 UI 布局检查与修复（2026-10-01）

本次针对用户提供的页面错位截图执行修复。沿用 Mantine、Tauri、Core、Virtuoso 和 Three.js；根据窄窗口需要调整工具栏与详情操作区，没有重写资产管理逻辑。

## 原因与修复

1. **打包后主题和 AppShell 布局样式被 CSP 拦截。** Tauri 给 HTML 中的 style 加入 nonce 后，浏览器会忽略同一 style-src 中的 unsafe-inline。Mantine 动态 style 未携带 nonce，主题退回蓝色，主区域偏移变量缺失。离屏添加与打包相同机制的 CSP 后复现：主区域 x=0、详情栏约 147 px、自定义薄荷色变量为空。修复后 1480 px 窗口下导航 248 px、主区域 928 px、详情栏 304 px。
   - 给已有启动样式加固定 id，使用 HTMLStyleElement.nonce 获取 Tauri 分配的文档 nonce。
   - 通过 MantineProvider.getStyleNonce 传给主题、响应式布局和浮层。
   - 使用现有 react-style-singleton 的 get-nonce 接口设置 Modal 滚动锁样式的 nonce。get-nonce 1.0.1 原已作为间接依赖安装，本次声明为直接依赖。
   - 启动 HTML 的内联属性样式改为已授权样式中的类。没有禁用 CSP。
2. **低高度窗口压扁导航和详情图。** 为两侧独立滚动区域的直接子项禁用 flex-shrink，内容过长时滚动，缩略图保持其比例。
3. **缩略图角标相互覆盖。** 收藏、镜头、非标准与预览状态放进可换行的标记区；批量选择时留出勾选位置，类型徽章不被文件名挤扁。非标准标记仅显示于模型。
4. **窄窗口标题、工具栏和详情操作拥挤。** 1250 px 以下采用两行顶部工具栏，标题与资产操作分行。详情底部操作改为整行按钮，标签文字换行、移除按钮改用紧凑 ActionIcon。
5. **弹窗聚焦后整页逐渐左移。** 原背景伪元素超过容器宽度，overflow:hidden 仍允许自动聚焦触发隐藏滚动。背景限制在容器横向范围内，根容器改为 overflow:clip；实际滚动继续由导航、Library、目录树和详情栏负责。
6. **动作查看器贴边、底部说明被裁切。** 三类查看器统一 24 px 外边距，并限制实际内容尺寸。Mantine 9 的 Modal.Content 会将 className 同时用于 inner 和 content，内容规则限定为 [data-modal-content]，避免误改定位层。
7. **滑块缺少可访问名称。** 卡片尺寸、点云尺寸和 VMD 时间轴改用 Mantine 的 thumbLabel，能够通过真实 slider 角色定位并键盘调整。

## 方法与验证边界

- 用户明确批准完全无窗口的 Edge + Playwright 离屏检查。使用新建临时浏览器上下文，运行构建后的 dist，不操作用户现有窗口。
- 仅本地模拟 Tauri IPC：48 项布局数据，显示 8536 项计数以覆盖较长数字；包含长中文/日文名称、长标签和扫描状态。没有连接真实数据库或执行素材操作。
- 缩略图使用此前 Core 生成的本地 Miki 图片。三类 3D 查看器使用合成三角形和单骨骼数据，验证布局与画布挂载；不代表真实 PMX/VMD 渲染、动作或相机验证。
- 视口为 1040×680、1250×780、1480×940、1920×1080，设备缩放系数 1。Windows 不同 DPI、原生 WebView2 和真实数据库冷启动耗时仍需用户实机检查。
- 检查主区域与左右栏边界、根容器横向滚动、页面横向溢出，以及关键工具栏、筛选行、详情区和弹窗的横向溢出；截图逐项复核。
- 本次没有测试真实 Move/Delete、全库缩略图入队、扫描暂停等数据操作，不将布局夹具结果表述为业务验证。

## 截图步骤

以下 30 个状态均已截图并检查；“通过”指上述离屏布局范围。截图保存在本地忽略目录，不随 Git 提交。

1. Library 常规窗口：**通过**。输出：[01-library-1480.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/01-library-1480.png)。
2. Library 最小窗口：**通过**。输出：[02-library-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/02-library-1040.png)。
3. 选中模型与详情顶部：**通过**，缩略图保持高度。输出：[03-inspector-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/03-inspector-1040.png)。
4. 详情底部独立滚动：**通过**，完整按钮与长标签可见。输出：[04-inspector-bottom-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/04-inspector-bottom-1040.png)。
5. 设置顶部：**通过**。输出：[05-settings-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/05-settings-1040.png)。
6. 多选长标签：**通过**，组合选择与清空按钮可换行。输出：[06-tags-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/06-tags-1040.png)。
7. 组合筛选面板：**通过**。输出：[07-filters-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/07-filters-1040.png)。
8. 扫描队列：**通过**，进度与动作区域不重叠。输出：[08-scan-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/08-scan-1040.png)。
9. 操作日志：**通过**。输出：[09-journal-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/09-journal-1040.png)。
10. 添加目录：**通过**，取消未提交添加。输出：[10-add-root-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/10-add-root-1040.png)。
11. 资产右键菜单：**通过**。输出：[11-context-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/11-context-1040.png)。
12. 查看缩略图：**通过**。输出：[12-thumbnail-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/12-thumbnail-1040.png)。
13. 批量选择：**通过**，勾选框与标记分开。输出：[13-bulk-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/13-bulk-1040.png)。
14. 目录树展开：**通过**，保留独立滚动区域。输出：[14-folder-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/14-folder-1040.png)。
15. 目录树收纳：**通过**，平滑改变高度。输出：[15-folder-compact-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/15-folder-compact-1040.png)。
16. 模型查看器：**通过布局检查**，合成网格。输出：[16-model-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/16-model-1040.png)。
17. 场景查看器：**通过布局检查**，合成网格。输出：[17-scene-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/17-scene-1040.png)。
18. 动作查看器：**通过布局检查**，合成网格与帧数据，底部说明完整。输出：[18-motion-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/18-motion-1040.png)。
19. Library 宽屏：**通过**。输出：[19-library-1920.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/19-library-1920.png)。
20. 启动阶段与等待提示：**通过布局检查**，阶段、当前行为、数量、耗时、无进展提示均为模拟数据。输出：[20-startup-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/20-startup-1040.png)。
21. 空资产库：**通过**，添加入口在内容滚动区中。输出：[20-empty-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/20-empty-1040.png)。
22. 重命名输入对话框：**通过**，取消未提交计划。输出：[21-prompt-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/21-prompt-1040.png)。
23. 文件操作计划：**通过布局检查**，夹具强制 canExecute=false，没有执行操作。输出：[22-operation-plan-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/22-operation-plan-1040.png)。
24. 智能集合删除确认：**通过**，取消未提交删除。输出：[23-confirm-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/23-confirm-1040.png)。
25. 设置底部滚动：**通过**，保存与数据库信息可见，关闭按钮保持可达。输出：[24-settings-bottom-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/24-settings-bottom-1040.png)。
26. 最小卡片尺寸 130：**通过**，键盘 Home 到达端点。输出：[25-card-min-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/25-card-min-1040.png)。
27. 最大卡片尺寸 300：**通过**，键盘 End 到达端点。输出：[26-card-max-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/26-card-max-1040.png)。
28. 1250 px 响应式边界：**通过**。输出：[27-library-1250.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/27-library-1250.png)。
29. 后台缩略图任务浮层：**通过**。输出：[28-jobs-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/28-jobs-1040.png)。
30. 根目录操作菜单：**通过**，运行中禁用项保持原行为。输出：[29-root-menu-1040.png](E:/MMD/MMDbridgeLib/target/ui-layout-audit/29-root-menu-1040.png)。

## 可复查记录

- `target/mantine-ui-probe/visual.cjs`：临时检查脚本，依赖位于忽略目录，不增加产品 Playwright 依赖。
- `node target/mantine-ui-probe/visual.cjs suite --packed --suite`：26 项主路径截图，加启动与空库两项截图，通过边界/溢出检查；无 CSP 和脚本错误。
- 最后一处查看器 CSS 选择器调整后，仅重新检查受影响的三类查看器并补拍两个浮层：`node target/mantine-ui-probe/visual.cjs recheck --packed --recheck`，5 项通过；三类查看器均满足 24 px 安全边距。
- `target/ui-layout-audit/suite-metrics.json` 与 `recheck-metrics.json`：实际边界与错误记录；查看器最终位置以 recheck 为准。
- TypeScript/Vite 构建通过；既有 ModelViewer >500 KB 提示仍存在，不是编译错误。
- 保留其他未提交改动；不提交、不推送、不生成安装器、不启动桌面程序。

## 便携交付

从 `apps/desktop` 离线执行 `npm run tauri -- build --no-bundle`，成功后将当前源码生成的 `mmdbridge-desktop.exe` 放到 `E:\MMD\MBL`。构建和复制结果记录于 `target/ui-layout-audit/portable-build.log`；不修改真实资产库。
