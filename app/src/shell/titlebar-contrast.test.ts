// 标题栏搜索入口（regular、wide 档的伪输入框）：放大镜与占位文字继承入口的字色，占位文字要被读到，不能用 text-3
// （日樱落在白底约 2.19:1），静止取 text-2；悬停照旧比静止提一级，提到 text。底是入口自己的实心底。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const css = readCss(new URL("./TitleBar.module.css", import.meta.url));
const shellCss = readCss(new URL("./AppShell.module.css", import.meta.url));

function ratio(appearance: AppearanceName, selector: ".search" | ".search:hover"): number {
  const tokens = tokensOf(appearance);
  const surface = resolveColor("var(--sk-surface)", tokens, [0, 0, 0]);
  const bg = resolveColor(declarationOf(css, ".search", "background"), tokens, surface);
  return contrast(resolveColor(declarationOf(css, selector, "color"), tokens, bg), bg);
}

describe("标题栏搜索入口的占位文字与放大镜", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：静止与悬停对入口底都不低于 4.5:1", (appearance) => {
    expect(ratio(appearance, ".search")).toBeGreaterThanOrEqual(4.5);
    expect(ratio(appearance, ".search:hover")).toBeGreaterThanOrEqual(4.5);
  });

  it("静止 text-2：日樱 5.63、夜樱 5.73；悬停 text：日樱 15.45、夜樱 13.40", () => {
    expect(declarationOf(css, ".search", "color")).toBe("var(--sk-text-2)");
    expect(declarationOf(css, ".search:hover", "color")).toBe("var(--sk-text)");
    expect(ratio("日樱 · 樱粉", ".search")).toBeCloseTo(5.63, 2);
    expect(ratio("夜樱 · 樱粉", ".search")).toBeCloseTo(5.73, 2);
    expect(ratio("日樱 · 樱粉", ".search:hover")).toBeCloseTo(15.45, 2);
    expect(ratio("夜樱 · 樱粉", ".search:hover")).toBeCloseTo(13.4, 2);
  });
});

// 窗口三键的字形是要认出来的图形，对所在的底至少 3:1。窗口失焦时不降色：降到 text-3 对窗口底只有日樱 2.06、夜樱 3.11，
// 叠上右上角的晕染后日樱低到 1.70、夜樱低到 2.06，失焦与聚焦一样取 text-2。
// 标题栏自己没有底色，三键落在应用根的窗口底色上，上面叠着背景层的氛围晕染：每一团晕染都按起始处（最浓处）的不透明度
// 叠到窗口底上，取其中最不利的一处。三键的类名只在 TitleBar.module.css 里有效（CSS 模块），失焦规则只需在这一份里查。

/** 氛围晕染的一团：起始色是「某色 N% 与 transparent」的径向渐变。 */
const GLOW = /radial-gradient\([^,]+, (color-mix\(in srgb, var\(--sk-[a-z0-9-]+\) \d+%, transparent\)), transparent \d+%\)/g;

/** 窗口底色，以及每一团晕染最浓处叠在窗口底上的颜色；有认不出的晕染就抛错，免得漏掉一团。 */
function windowGrounds(tokens: Map<string, string>): { base: Rgb; glows: Rgb[] } {
  const base = resolveColor(declarationOf(shellCss, ".app", "background"), tokens, [0, 0, 0]);
  const ref = /^var\((--sk-[a-z0-9-]+)\)$/.exec(declarationOf(shellCss, ".backdrop", "background"));
  if (ref === null) throw new Error("背景层的底不是单个令牌");
  const ambience = tokens.get(ref[1]);
  if (ambience === undefined) throw new Error(`令牌 ${ref[1]} 没有定义`);
  const glows = [...ambience.matchAll(GLOW)].map((m) => resolveColor(m[1], tokens, base));
  if (glows.length === 0 || glows.length !== ambience.split("radial-gradient(").length - 1) throw new Error(`认不全氛围晕染：${ambience}`);
  return { base, glows };
}

function keyRatios(appearance: AppearanceName): { base: number; worst: number } {
  const tokens = tokensOf(appearance);
  const { base, glows } = windowGrounds(tokens);
  const ink = resolveColor(declarationOf(css, ".winButton", "color"), tokens, base);
  return { base: contrast(ink, base), worst: Math.min(contrast(ink, base), ...glows.map((glow) => contrast(ink, glow))) };
}

const KEY_RATIOS = {
  "日樱 · 樱粉": { base: 5.3, worst: 4.48 },
  "日樱 · 紫水晶": { base: 5.3, worst: 4.38 },
  "日樱 · 青苔": { base: 5.3, worst: 4.67 },
  "夜樱 · 樱粉": { base: 6.46, worst: 4.96 },
  "夜樱 · 紫水晶": { base: 6.46, worst: 4.33 },
  "夜樱 · 青苔": { base: 6.46, worst: 4.28 },
} as const satisfies Record<AppearanceName, { base: number; worst: number }>;

describe("标题栏窗口三键的字形", () => {
  it("失焦态只给标题与面包屑降色，三键静止取 text-2，失焦与聚焦相同", () => {
    const blurred = [...css.matchAll(/([^{}]+)\{[^{}]*\}/g)].flatMap((rule) => rule[1].split(",").map((s) => s.trim())).filter((s) => s.includes("[data-blurred]"));
    expect(blurred).toEqual([".bar[data-blurred] .title", ".bar[data-blurred] .crumbs"]);
    expect(declarationOf(css, ".winButton", "color")).toBe("var(--sk-text-2)");
  });

  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：对窗口底与晕染最浓处都不低于 3:1，比值钉住", (appearance) => {
    const { base, worst } = keyRatios(appearance);
    expect(worst).toBeGreaterThanOrEqual(3);
    expect(base).toBeCloseTo(KEY_RATIOS[appearance].base, 2);
    expect(worst).toBeCloseTo(KEY_RATIOS[appearance].worst, 2);
  });
});
