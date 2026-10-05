import { SegmentedControl, Switch, useMantineColorScheme, type MantineColorScheme } from "@mantine/core";
import { Monitor, Moon, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { readPreference, writePreference } from "./preferences";

function storedBoolean(key: string, fallback: boolean) {
  const stored = readPreference(key);
  return stored === null ? fallback : stored !== "false";
}

export function applyAppearancePreferences() {
  document.documentElement.dataset.libraryTransparency = storedBoolean("mmdbridge-transparency", true) ? "on" : "off";
  document.documentElement.dataset.libraryMotion = storedBoolean("mmdbridge-motion", true) ? "on" : "off";
}

export function AppearanceSettings() {
  const { colorScheme, setColorScheme } = useMantineColorScheme();
  const [transparency, setTransparency] = useState(() => storedBoolean("mmdbridge-transparency", true));
  const [motion, setMotion] = useState(() => storedBoolean("mmdbridge-motion", true));
  useEffect(() => { applyAppearancePreferences(); }, []);

  function save(key: string, value: boolean, setter: (value: boolean) => void) {
    setter(value);
    writePreference(key, String(value));
    if (key === "mmdbridge-transparency") document.documentElement.dataset.libraryTransparency = value ? "on" : "off";
    else document.documentElement.dataset.libraryMotion = value ? "on" : "off";
  }

  return <section className="settings-field appearance-settings" aria-label="界面外观">
    <label>界面外观</label>
    <p>选择浅色、深色或跟随系统。</p>
    <SegmentedControl fullWidth aria-label="颜色主题" value={colorScheme} onChange={(value) => setColorScheme(value as MantineColorScheme)} data={[
      { value: "auto", label: <span className="appearance-choice"><Monitor size={16} aria-hidden="true" />跟随系统</span> },
      { value: "light", label: <span className="appearance-choice"><Sun size={16} aria-hidden="true" />浅色</span> },
      { value: "dark", label: <span className="appearance-choice"><Moon size={16} aria-hidden="true" />深色</span> },
    ]} />
    <div className="appearance-option"><div><strong>透明效果</strong><span>侧栏和浮层使用磨砂玻璃；关闭后使用实色材质。</span></div><Switch checked={transparency} aria-label="透明效果" onChange={(event) => save("mmdbridge-transparency", event.currentTarget.checked, setTransparency)} /></div>
    <div className="appearance-option"><div><strong>界面动效</strong><span>悬停与窗口过渡；系统开启减少动态效果时自动停用。</span></div><Switch checked={motion} aria-label="界面动效" onChange={(event) => save("mmdbridge-motion", event.currentTarget.checked, setMotion)} /></div>
  </section>;
}
