// 命令面板里要被看到的文字与图形：输入行的放大镜、输入占位、分组标题、结果行尾的提示（版本与加载器、「当前实例」、
// 快捷键、账户类型、没法登录的原因）与底栏的按键说明。它们都不是装饰也不是禁用态，不能用 text-3（日樱落在白底约 2.19:1）：
// 文字对所在的底至少 4.5:1，放大镜是图形，至少 3:1。行尾提示既落在面板底上，也落在高亮行的 accent-soft 上，两种底都要过。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const css = readCss(new URL("./CommandPalette.module.css", import.meta.url));

/** 面板正文、高亮的结果行、底栏三种底。 */
type Ground = "panel" | "active" | "foot";

const SEEN: readonly { selector: string; grounds: readonly Ground[]; min: number }[] = [
  { selector: ".inputRow", grounds: ["panel"], min: 3 },
  { selector: ".input::placeholder", grounds: ["panel"], min: 4.5 },
  { selector: ".groupLabel", grounds: ["panel"], min: 4.5 },
  { selector: ".optionHint", grounds: ["panel", "active"], min: 4.5 },
  { selector: ".foot", grounds: ["foot"], min: 4.5 },
];

function groundColor(ground: Ground, tokens: Map<string, string>): Rgb {
  const surface = resolveColor("var(--sk-surface)", tokens, [0, 0, 0]);
  const panel = resolveColor(declarationOf(css, ".panel", "background"), tokens, surface);
  if (ground === "panel") return panel;
  if (ground === "active") return resolveColor(declarationOf(css, ".option[data-active]", "background"), tokens, panel);
  return resolveColor(declarationOf(css, ".foot", "background"), tokens, panel);
}

function ratio(appearance: AppearanceName, selector: string, ground: Ground): number {
  const tokens = tokensOf(appearance);
  const bg = groundColor(ground, tokens);
  return contrast(resolveColor(declarationOf(css, selector, "color"), tokens, bg), bg);
}

describe("命令面板里要被看到的文字与图形", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：每一处对所在的底都过线（文字 4.5:1，放大镜 3:1）", (appearance) => {
    const failing = SEEN.flatMap(({ selector, grounds, min }) =>
      grounds.map((ground) => ({ selector, ground, min, ratio: Number(ratio(appearance, selector, ground).toFixed(2)) })).filter((r) => r.ratio < r.min),
    );
    expect(failing).toEqual([]);
  });

  it("全部取次要文字 text-2", () => {
    expect(SEEN.map(({ selector }) => [selector, declarationOf(css, selector, "color")])).toEqual(SEEN.map(({ selector }) => [selector, "var(--sk-text-2)"]));
  });

  it("实际对比度：面板底日樱 5.63、夜樱 5.73，底栏 4.90、5.94，高亮行最低是日樱樱粉 4.80、夜樱青苔 4.67", () => {
    expect(ratio("日樱 · 樱粉", ".input::placeholder", "panel")).toBeCloseTo(5.63, 2);
    expect(ratio("夜樱 · 樱粉", ".input::placeholder", "panel")).toBeCloseTo(5.73, 2);
    expect(ratio("日樱 · 樱粉", ".foot", "foot")).toBeCloseTo(4.9, 2);
    expect(ratio("夜樱 · 樱粉", ".foot", "foot")).toBeCloseTo(5.94, 2);
    expect(ratio("日樱 · 樱粉", ".optionHint", "active")).toBeCloseTo(4.8, 2);
    expect(ratio("日樱 · 紫水晶", ".optionHint", "active")).toBeCloseTo(4.88, 2);
    expect(ratio("日樱 · 青苔", ".optionHint", "active")).toBeCloseTo(4.89, 2);
    expect(ratio("夜樱 · 樱粉", ".optionHint", "active")).toBeCloseTo(4.87, 2);
    expect(ratio("夜樱 · 紫水晶", ".optionHint", "active")).toBeCloseTo(4.78, 2);
    expect(ratio("夜樱 · 青苔", ".optionHint", "active")).toBeCloseTo(4.67, 2);
  });
});
