import { Menu } from "@mantine/core";
import type { ReactNode } from "react";

export function LibraryContextMenu({ x, y, label, onClose, children, width = 244 }: {
  x: number; y: number; label: string; onClose: () => void; children: ReactNode; width?: number;
}) {
  return <Menu opened onChange={(opened) => { if (!opened) onClose(); }} position="bottom-start" offset={0} width={width} withinPortal zIndex={300} returnFocus={false}>
    <Menu.Target><span aria-hidden="true" style={{ position: "fixed", left: x, top: y, width: 1, height: 1 }} /></Menu.Target>
    <Menu.Dropdown aria-label={label} style={{ maxHeight: "calc(100vh - 16px)", overflowY: "auto" }}>{children}</Menu.Dropdown>
  </Menu>;
}
