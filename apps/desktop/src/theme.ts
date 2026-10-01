import { Button, Checkbox, createTheme, Modal, NativeSelect, TextInput } from "@mantine/core";

// Shared by Library, startup and both 3D viewers.
export const libraryTheme = createTheme({
  primaryColor: "mint",
  primaryShade: 5,
  colors: {
    mint: ["#e5faf3", "#c5f2e2", "#9de7cc", "#78dcba", "#5ed6b2", "#3fbf99", "#2e9e7e", "#247f67", "#1d6554", "#174f44"],
    dark: ["#d8e5e2", "#bbceca", "#91aaa6", "#6d8784", "#405957", "#2c4246", "#20323b", "#17262f", "#121d25", "#0e161d"],
  },
  fontFamily: '"Segoe UI", "Microsoft YaHei UI", sans-serif',
  fontFamilyMonospace: 'Consolas, "Cascadia Code", monospace',
  fontSizes: { xs: "11px", sm: "12px", md: "13px", lg: "15px", xl: "18px" },
  defaultRadius: "sm",
  breakpoints: { xs: "36em", sm: "48em", md: "66.25em", lg: "78.125em", xl: "100em" },
  focusRing: "auto",
  components: {
    Button: Button.extend({ defaultProps: { size: "xs", variant: "light" } }),
    TextInput: TextInput.extend({ defaultProps: { size: "xs" } }),
    NativeSelect: NativeSelect.extend({ defaultProps: { size: "xs" } }),
    Checkbox: Checkbox.extend({ defaultProps: { size: "xs" } }),
    Modal: Modal.extend({ defaultProps: { centered: true, overlayProps: { backgroundOpacity: 0.72, blur: 6 }, transitionProps: { duration: 150 } } }),
  },
});
