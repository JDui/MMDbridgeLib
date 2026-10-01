import { Button, Checkbox, createTheme, Modal, NativeSelect, TextInput, type CSSVariablesResolver } from "@mantine/core";

const mintPalette = ["#e5faf3", "#c5f2e2", "#9de7cc", "#78dcba", "#5ed6b2", "#3fbf99", "#2e9e7e", "#247f67", "#1d6554", "#174f44"] as const;
const libraryAccent = {
  mint: mintPalette[4], mintLight: "#93e9cd", mintStrong: mintPalette[5], mintVeil: "rgba(94, 214, 178, .12)",
  violet: "#c2a1ef", violetVeil: "rgba(194, 161, 239, .12)",
  amber: "#e7b06d", amberVeil: "rgba(231, 176, 109, .12)",
  rose: "#ef9a95", roseVeil: "rgba(239, 154, 149, .12)", blue: "#87b5f0",
} as const;

// Product CSS and Mantine share these values through the provider's nonce-bearing stylesheet.
export const libraryTokens = {
  surface: {
    background: "#0e141b", sidebar: "#131d26", panel: "#131c25", secondary: "#17222c",
    elevated: "#1c2935", hover: "#213040", selected: "#193730", overlay: "rgba(14, 20, 27, .88)",
  },
  text: { primary: "#e9eff4", secondary: "#b8c4cf", tertiary: "#9aa8b5", muted: "#8b99a7", disabled: "#6d8784" },
  border: { subtle: "#233040", default: "#2d3e50", strong: "#3c5165" },
  accent: libraryAccent,
  semantic: {
    success: libraryAccent.mint, warning: libraryAccent.amber, error: libraryAccent.rose, info: libraryAccent.blue,
    scanning: libraryAccent.mint, stale: libraryAccent.amber, missing: libraryAccent.rose,
  },
  font: {
    ui: '"Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", "Microsoft YaHei", system-ui, sans-serif',
    mono: '"Cascadia Mono", "Cascadia Code", Consolas, "Sarasa Mono SC", "Microsoft YaHei UI", monospace',
    size: { xs: "11px", sm: "12px", md: "13px", lg: "15px", xl: "18px" },
  },
  radius: { xs: "5px", sm: "8px", md: "11px", lg: "16px", xl: "20px" },
  spacing: { xs: "10px", sm: "12px", md: "16px", lg: "20px", xl: "32px" },
  controlHeight: { xs: "30px", sm: "36px", md: "42px", lg: "50px", xl: "60px" },
  shadow: {
    card: "0 1px 2px rgba(4, 9, 13, .4)", raised: "0 8px 24px rgba(4, 9, 13, .45)",
    panel: "0 18px 50px rgba(3, 7, 11, .55)", overlay: "0 28px 80px rgba(2, 5, 9, .7)",
    ring: "0 0 0 2px rgba(94, 214, 178, .18)",
  },
} as const;

export const libraryCssVariablesResolver: CSSVariablesResolver = (theme) => ({
  variables: {
    "--library-bg": libraryTokens.surface.background,
    "--library-sidebar": libraryTokens.surface.sidebar,
    "--library-panel": libraryTokens.surface.panel,
    "--library-panel-secondary": libraryTokens.surface.secondary,
    "--library-elevated": libraryTokens.surface.elevated,
    "--library-hover": libraryTokens.surface.hover,
    "--library-selected": libraryTokens.surface.selected,
    "--library-overlay": libraryTokens.surface.overlay,
    "--library-text": libraryTokens.text.primary,
    "--library-text-secondary": libraryTokens.text.secondary,
    "--library-text-tertiary": libraryTokens.text.tertiary,
    "--library-text-muted": libraryTokens.text.muted,
    "--library-text-disabled": libraryTokens.text.disabled,
    "--library-border-subtle": libraryTokens.border.subtle,
    "--library-border": libraryTokens.border.default,
    "--library-border-strong": libraryTokens.border.strong,
    "--library-mint": libraryTokens.accent.mint,
    "--library-mint-light": libraryTokens.accent.mintLight,
    "--library-mint-strong": libraryTokens.accent.mintStrong,
    "--library-mint-veil": libraryTokens.accent.mintVeil,
    "--library-violet": libraryTokens.accent.violet,
    "--library-violet-veil": libraryTokens.accent.violetVeil,
    "--library-amber": libraryTokens.accent.amber,
    "--library-amber-veil": libraryTokens.accent.amberVeil,
    "--library-rose": libraryTokens.accent.rose,
    "--library-rose-veil": libraryTokens.accent.roseVeil,
    "--library-blue": libraryTokens.accent.blue,
    "--library-success": libraryTokens.semantic.success,
    "--library-warning": libraryTokens.semantic.warning,
    "--library-error": libraryTokens.semantic.error,
    "--library-info": libraryTokens.semantic.info,
    "--library-scanning": libraryTokens.semantic.scanning,
    "--library-stale": libraryTokens.semantic.stale,
    "--library-missing": libraryTokens.semantic.missing,
    "--library-font-ui": theme.fontFamily,
    "--library-font-mono": theme.fontFamilyMonospace,
    "--library-radius-xs": theme.radius.xs,
    "--library-radius-sm": theme.radius.sm,
    "--library-radius-md": theme.radius.md,
    "--library-radius-lg": theme.radius.lg,
    "--library-control-xs": libraryTokens.controlHeight.xs,
    "--library-control-sm": libraryTokens.controlHeight.sm,
    "--library-control-md": libraryTokens.controlHeight.md,
    "--library-control-lg": libraryTokens.controlHeight.lg,
    "--library-control-xl": libraryTokens.controlHeight.xl,
    "--library-shadow-card": libraryTokens.shadow.card,
    "--library-shadow-raised": libraryTokens.shadow.raised,
    "--library-shadow-panel": libraryTokens.shadow.panel,
    "--library-shadow-overlay": libraryTokens.shadow.overlay,
    "--library-ring": libraryTokens.shadow.ring,
  },
  dark: {
    "--mantine-color-body": "var(--library-bg)",
    "--mantine-color-text": "var(--library-text)",
    "--mantine-color-dimmed": "var(--library-text-tertiary)",
  },
  light: {},
});

// Shared by Library, startup and both 3D viewers.
export const libraryTheme = createTheme({
  primaryColor: "mint",
  primaryShade: 5,
  colors: {
    mint: [...mintPalette],
    dark: [libraryTokens.text.primary, libraryTokens.text.secondary, libraryTokens.text.tertiary, libraryTokens.text.disabled,
      "#405957", "#2c4246", "#20323b", libraryTokens.surface.secondary, libraryTokens.surface.panel, libraryTokens.surface.background],
  },
  fontFamily: libraryTokens.font.ui,
  fontFamilyMonospace: libraryTokens.font.mono,
  fontSizes: libraryTokens.font.size,
  radius: libraryTokens.radius,
  spacing: libraryTokens.spacing,
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
