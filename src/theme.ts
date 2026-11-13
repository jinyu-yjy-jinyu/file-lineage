/**
 * Single source of truth for the visual tokens shared between the DOM and the
 * canvas.
 *
 * M14: the lineage graph used to hold its own colour table while `App.tsx`
 * hard-coded a *second* table for the legend, so the two could disagree. The
 * canvas cannot resolve `var(--primary)`, so it reads the CSS custom properties
 * through `getComputedStyle`; the legend uses the same keys and lets CSS resolve
 * them. Both come from here.
 */

export interface GraphPalette {
  nodes: Record<string, string>;
  edges: Record<string, string>;
  label: string;
  halo: string;
  muted: string;
}

function cssVar(name: string, fallback: string): string {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return fallback;
  }
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return value || fallback;
}

function withAlpha(color: string, alpha: number): string {
  const match = /^#([0-9a-f]{6})$/i.exec(color.trim());
  if (!match) {
    return color;
  }
  const value = Number.parseInt(match[1], 16);
  const r = (value >> 16) & 255;
  const g = (value >> 8) & 255;
  const b = value & 255;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/** Concrete colours for the canvas, derived from the CSS custom properties. */
export function graphPalette(): GraphPalette {
  const primary = cssVar("--primary", "#5e6ad2");
  const inkMuted = cssVar("--ink-muted", "#d0d6e0");
  const inkSubtle = cssVar("--ink-subtle", "#8a8f98");
  const hairlineStrong = cssVar("--hairline-strong", "#34343a");
  const surface4 = cssVar("--surface-4", "#191a1b");
  const ink = cssVar("--ink", "#f7f8f8");
  const canvas = cssVar("--canvas", "#010102");

  return {
    nodes: {
      file: primary,
      source: inkMuted,
      version: hairlineStrong,
      process: inkSubtle,
      copy: surface4,
    },
    edges: {
      origin: withAlpha(primary, 0.65),
      evolve: withAlpha(hairlineStrong, 0.75),
      touch: withAlpha(inkSubtle, 0.6),
      copy: withAlpha(hairlineStrong, 0.75),
    },
    label: ink,
    halo: withAlpha(canvas, 0.92),
    muted: inkSubtle,
  };
}

export interface LegendItem {
  key: string;
  label: string;
  /** CSS colour for the DOM legend; the canvas resolves the same key. */
  css: string;
}

/** Legend rows; `key` maps into `graphPalette().nodes`. */
export const graphLegend: LegendItem[] = [
  { key: "source", label: "来源", css: "var(--ink-muted)" },
  { key: "file", label: "文件", css: "var(--primary)" },
  { key: "version", label: "版本", css: "var(--hairline-strong)" },
  { key: "process", label: "进程", css: "var(--ink-subtle)" },
  { key: "copy", label: "副本", css: "var(--surface-4)" },
];

/** `Q1` … `high/medium/low` → the shared gauge glyphs. */
export function confidenceGlyphs(confidence: string): string {
  if (confidence === "high") return "●●●";
  if (confidence === "medium") return "●●○";
  return "●○○";
}
