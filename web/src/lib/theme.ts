import type { Palette } from "./types";

const VARS: Record<string, string> = {
  bg: "--bg",
  bgRaised: "--bg-raised",
  bgHover: "--bg-hover",
  text: "--text",
  textSecondary: "--text-secondary",
  grayDim: "--gray-dim",
  gray: "--gray",
  grayBright: "--gray-bright",
  accent: "--accent",
  accentThinking: "--accent-thinking",
  success: "--success",
  error: "--error",
  warning: "--warning",
  info: "--info",
  path: "--path",
  command: "--command",
  link: "--link",
  model: "--model",
  plan: "--plan",
  border: "--border",
  borderActive: "--border-active",
  diffInsBg: "--diff-ins-bg",
  diffInsFg: "--diff-ins-fg",
  diffDelBg: "--diff-del-bg",
  diffDelFg: "--diff-del-fg",
  h1: "--h1",
  h2: "--h2",
  h3: "--h3",
  h4: "--h4",
  codeInline: "--code-inline",
  mdText: "--md-text",
  mdBullet: "--md-bullet",
  mdStrong: "--md-strong",
  mdQuoteBar: "--md-quote-bar",
  mdQuoteText: "--md-quote-text",
  mdRule: "--md-rule",
};

export function applyPalette(p: Palette | undefined) {
  if (!p) return;
  const root = document.documentElement;
  for (const [k, v] of Object.entries(VARS)) {
    if (typeof p[k] === "string") root.style.setProperty(v, p[k]);
  }
  root.style.colorScheme = p.light ? "light" : "dark";
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", p.bg ?? "#1b1b1f");
}

const KEY = "blush.theme";
export const savedTheme = () => localStorage.getItem(KEY);
export const saveTheme = (name: string | null) => (name ? localStorage.setItem(KEY, name) : localStorage.removeItem(KEY));
