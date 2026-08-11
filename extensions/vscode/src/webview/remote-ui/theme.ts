import type { ThemeTokens, UiJsonValue } from "@codypendent/ui";
import type { UiWireTheme } from "../../remote-ui/wire.js";

const SAFE_TOKEN = /^[a-zA-Z][a-zA-Z0-9._-]{0,127}$/;
const SAFE_COLOR = /^(#[0-9a-fA-F]{3,8}|(?:rgb|hsl)a?\([0-9.,% /+-]+\)|(?:transparent|currentColor|inherit))$/;

function cssTokenName(name: string): string {
  return `--cody-ui-${name.replace(/[._]/g, "-").toLowerCase()}`;
}

function safeScalar(value: UiJsonValue): string | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  if (typeof value === "boolean") return value ? "1" : "0";
  if (typeof value !== "string" || value.length > 256) return undefined;
  // Theme values are data, never arbitrary CSS. This explicitly blocks url(),
  // var(), declarations, and other constructs that could escape the token.
  if (/url\s*\(|var\s*\(|[;{}]/i.test(value)) return undefined;
  return value;
}

function clearThemeProperties(element: HTMLElement): void {
  for (let index = element.style.length - 1; index >= 0; index -= 1) {
    const name = element.style.item(index);
    if (name.startsWith("--cody-ui-")) element.style.removeProperty(name);
  }
}

export function applyWireTheme(element: HTMLElement, theme: UiWireTheme): void {
  clearThemeProperties(element);
  element.dataset.uiTheme = theme.id;
  element.dataset.uiColorScheme = theme.colorScheme ?? "auto";
  element.dataset.uiHighContrast = String(theme.highContrast ?? false);
  element.dataset.uiReducedMotion = String(theme.reducedMotion ?? false);
  for (const [name, value] of Object.entries(theme.tokens ?? {})) {
    if (!SAFE_TOKEN.test(name)) continue;
    const scalar = name.startsWith("spacing.") && typeof value === "number"
      ? `${Math.max(0, Math.min(256, value))}px`
      : safeScalar(value);
    if (scalar !== undefined) element.style.setProperty(cssTokenName(name), scalar);
  }
}

export function applyThemeTokens(element: HTMLElement, theme: ThemeTokens): void {
  clearThemeProperties(element);
  element.dataset.uiTheme = theme.id;
  element.dataset.uiColorScheme = theme.mode;
  for (const [name, value] of Object.entries(theme.colors)) {
    if (SAFE_TOKEN.test(name) && SAFE_COLOR.test(value)) {
      element.style.setProperty(cssTokenName(name), value);
    }
  }
  for (const [name, value] of Object.entries(theme.spacing)) {
    if (SAFE_TOKEN.test(name) && Number.isFinite(value) && value >= 0 && value <= 256) {
      element.style.setProperty(cssTokenName(`spacing.${name}`), `${value}px`);
    }
  }
}
