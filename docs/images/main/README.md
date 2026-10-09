# 当前 README 展示图

截图日期：2026-10-08。功能源码提交：`bd2f76c24a583110e24fbae18907525d9014902c`。

图片直接使用该提交的 CI 原始 JPG。截图环境为 Linux、隔离的无窗口 Chromium 和软件 Vulkan / WebGL；前端使用测试 Tauri 桥，模型预览和缩略图数据由实际 Rust Core 生成。全部素材是合成测试样例，不包含用户资产。

| 图片 | 内容 | 原始 CI 文件 |
| --- | --- | --- |
| [library-dark.jpg](library-dark.jpg) | 玻璃资产库与角色主体缩略图 | `MBL_Subject_Stage4_Library_After.jpg` |
| [library-light.jpg](library-light.jpg) | 浅色资产库与材质兼容性样例 | `MBL_Model_Stage3_Library_Light.jpg` |
| [character-matcap.jpg](character-matcap.jpg) | 角色白瓷 Matcap 与权重检查入口 | `MBL_Viewer_Stage1_Character_Matcap.jpg` |
| [motion-matcap.jpg](motion-matcap.jpg) | 动作银灰 Matcap 与时间轴 | `MBL_Viewer_Stage2_Motion_Matcap.jpg` |
| [scene-reference.jpg](scene-reference.jpg) | 冷色夜景与原点参照角色 | `MBL_Viewer_Stage4_Scene_Night.jpg` |
| [automatic-tags.jpg](automatic-tags.jpg) | 技术标签与角色整体色 | `MBL_Tags_Stage2_Colours.jpg` |
| [agentlink-working.jpg](agentlink-working.jpg) | AgentLink Prompt / Log 与工作状态 | `MBL_AgentLink_Motion_Dark.jpg` |
| [thumbnail-framing.jpg](thumbnail-framing.jpg) | 异常模型旧版 / 当前主体取景对比 | `MBL_Subject_Stage1_Outliers.jpg` |

主体取景截图来自 [Character subject framing 37714140097](https://github.com/JDui/MMDbridgeLib/actions/runs/37714140097)。该流程用同一组合成输入比较旧版渲染器 `03ae412` 和当前渲染器；`before` 列为旧版，`after` 列为当前版。

其余截图来自 [Model compatibility and offscreen rendering 37714140077](https://github.com/JDui/MMDbridgeLib/actions/runs/37714140077)。AgentLink 画面采用实际前端与合成 Core 状态轨迹，不代表外部 Agent 正在分类用户模型。

两条管线均通过，隔离页面异常为零。图片只展示对应功能和构图，Windows 实机字体、显卡性能以及真实复杂模型的视觉效果仍需单独验证。
