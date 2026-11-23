// 样式守卫用的对比度计算：jsdom 不算样式，这里直接读样式表，按 styles/tokens.css 在各外观下解出令牌的实际颜色，
// 再按 WCAG 2.x 相对亮度公式算对比度（半透明色先与它实际所在的底合成）。只被 *.test.ts 引用。

import { readFileSync } from "node:fs";

export type Rgb = readonly [number, number, number];

/** 读样式表并去掉注释；地址由调用方按自己的位置给出（new URL("./x.module.css", import.meta.url)）。 */
export function readCss(url: URL): string {
  return readFileSync(url, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
}

/** 按顶层逗号拆选择器列表：:not(a, b) 这类括号里的逗号不拆。 */
function splitSelectorList(list: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < list.length; i++) {
    const c = list[i];
    if (c === "(") depth++;
    else if (c === ")") depth--;
    else if (c === "," && depth === 0) {
      parts.push(list.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(list.slice(start));
  return parts;
}

/**
 * 一个选择器的生效声明：选择器列表里含它的各条顶层规则按出现顺序合并，后面的覆盖前面的。
 * 只认没有嵌套的「选择器 { 声明 }」，也不比特异性，适用于单类名与它的逗号分组这类特异性相同的规则。
 */
export function declarationsOf(css: string, selector: string): Map<string, string> {
  const out = new Map<string, string>();
  let found = false;
  for (const m of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    if (!splitSelectorList(m[1]).some((s) => s.trim() === selector)) continue;
    found = true;
    for (const part of m[2].split(";")) {
      const at = part.indexOf(":");
      if (at >= 0) out.set(part.slice(0, at).trim(), part.slice(at + 1).trim().replace(/\s+/g, " "));
    }
  }
  if (!found) throw new Error(`样式表里没有选择器 ${selector}`);
  return out;
}

export function declarationOf(css: string, selector: string, property: string): string {
  const value = declarationsOf(css, selector).get(property);
  if (value === undefined) throw new Error(`${selector} 没有声明 ${property}`);
  return value;
}

const tokensCss = readCss(new URL("../styles/tokens.css", import.meta.url));

/** 两套明暗与三种强调色的组合，各自按顺序叠加的令牌块。 */
export const APPEARANCES = {
  "日樱 · 樱粉": [":root"],
  "日樱 · 紫水晶": [":root", ':root[data-sk-accent="amethyst"]'],
  "日樱 · 青苔": [":root", ':root[data-sk-accent="moss"]'],
  "夜樱 · 樱粉": [":root", ':root[data-sk-mode="night"]'],
  "夜樱 · 紫水晶": [":root", ':root[data-sk-accent="amethyst"]', ':root[data-sk-mode="night"]', ':root[data-sk-mode="night"][data-sk-accent="amethyst"]'],
  "夜樱 · 青苔": [":root", ':root[data-sk-accent="moss"]', ':root[data-sk-mode="night"]', ':root[data-sk-mode="night"][data-sk-accent="moss"]'],
} as const satisfies Record<string, readonly string[]>;

export type AppearanceName = keyof typeof APPEARANCES;

/** 一种外观下全部令牌的取值。 */
export function tokensOf(appearance: AppearanceName): Map<string, string> {
  const merged = new Map<string, string>();
  for (const selector of APPEARANCES[appearance]) {
    for (const [k, v] of declarationsOf(tokensCss, selector)) merged.set(k, v);
  }
  return merged;
}

function hex(value: string): Rgb {
  const m = /^#([0-9a-fA-F]{6})$/.exec(value);
  if (m === null) throw new Error(`不是六位十六进制色值：${value}`);
  return [0, 2, 4].map((i) => parseInt(m[1].slice(i, i + 2), 16)) as unknown as Rgb;
}

/**
 * 把颜色值算成不透明色：纯色直接取；「某色 N% 与另一色」按 sRGB 线性插值；「某色 N% 与 transparent」与带透明度的 rgba
 * 写法按各自的不透明度叠到 backdrop 上。认不出的写法直接抛错，不猜。
 */
export function resolveColor(value: string, tokens: Map<string, string>, backdrop: Rgb): Rgb {
  const ref = /^var\((--sk-[a-z0-9-]+)\)$/.exec(value);
  if (ref !== null) {
    const next = tokens.get(ref[1]);
    if (next === undefined) throw new Error(`令牌 ${ref[1]} 没有定义`);
    return resolveColor(next, tokens, backdrop);
  }
  // 卡片底与悬停、按下叠层（--sk-card、--sk-fill-hover、--sk-fill-press）是 rgba 写法
  const rgba = /^rgba\((\d+), (\d+), (\d+), (\d*\.?\d+)\)$/.exec(value);
  if (rgba !== null) {
    const alpha = Number(rgba[4]);
    return [rgba[1], rgba[2], rgba[3]].map((c, i) => Number(c) * alpha + backdrop[i] * (1 - alpha)) as unknown as Rgb;
  }
  const fade = /^color-mix\(in srgb, (var\(--sk-[a-z0-9-]+\)) (\d+)%, transparent\)$/.exec(value);
  if (fade !== null) {
    const color = resolveColor(fade[1], tokens, backdrop);
    const alpha = Number(fade[2]) / 100;
    return color.map((c, i) => c * alpha + backdrop[i] * (1 - alpha)) as unknown as Rgb;
  }
  const blend = /^color-mix\(in srgb, (var\(--sk-[a-z0-9-]+\)) (\d+)%, (var\(--sk-[a-z0-9-]+\))\)$/.exec(value);
  if (blend !== null) {
    const first = resolveColor(blend[1], tokens, backdrop);
    const second = resolveColor(blend[3], tokens, backdrop);
    const weight = Number(blend[2]) / 100;
    return first.map((c, i) => c * weight + second[i] * (1 - weight)) as unknown as Rgb;
  }
  return hex(value);
}

const channel = (c: number) => {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
};

const luminance = ([r, g, b]: Rgb) => 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);

export function contrast(a: Rgb, b: Rgb): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}
