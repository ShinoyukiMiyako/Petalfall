// 账户胶囊 wide 档的账户类型行，与切换菜单里每行的第二行（账户类型、「需要重新登录」、「管理账户」的说明），都是要被读到的信息：
// 不能用 text-3（落在白底约 2.19:1），对所在的底至少 4.5:1。菜单行悬停与键盘高亮铺 --sk-fill-hover 叠层，text-2 在叠层上
// 不到 4.5:1，那时提到 text；当前账户行的底是 accent-soft、不铺叠层，需要重新登录的那一行在叠层上仍用警示色。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const css = readCss(new URL("./AccountCapsule.module.css", import.meta.url));
const popoverCss = readCss(new URL("../ui/Popover.module.css", import.meta.url));

/** 悬停与键盘高亮时把第二行提到 text 的两条选择器（同一条规则）。 */
const LIFTED = [".item:hover:not([data-current]) .itemLine:not([data-expired])", ".item:focus-visible:not([data-current]) .itemLine:not([data-expired])"] as const;

/** 胶囊上的类型行；菜单行静止、当前账户行、悬停或键盘高亮的行；需要重新登录的行被悬停。 */
type Spot = "capsule" | "rest" | "current" | "highlighted" | "expiredHighlighted";

const SPOTS: readonly Spot[] = ["capsule", "rest", "current", "highlighted", "expiredHighlighted"];

function groundOf(spot: Spot, tokens: Map<string, string>): Rgb {
  if (spot === "capsule") return resolveColor(declarationOf(css, ".capsule", "background"), tokens, [0, 0, 0]);
  const menu = resolveColor(declarationOf(popoverCss, ".panel", "background"), tokens, [0, 0, 0]);
  if (spot === "rest") return menu;
  if (spot === "current") return resolveColor(declarationOf(css, ".item[data-current]", "background"), tokens, menu);
  return resolveColor(declarationOf(css, ".item:hover", "background"), tokens, menu);
}

function colorOf(spot: Spot): string {
  if (spot === "capsule") return declarationOf(css, ".kind", "color");
  if (spot === "highlighted") return declarationOf(css, LIFTED[0], "color");
  if (spot === "expiredHighlighted") return declarationOf(css, ".itemLine[data-expired]", "color");
  return declarationOf(css, ".itemLine", "color");
}

function ratio(appearance: AppearanceName, spot: Spot): number {
  const tokens = tokensOf(appearance);
  const bg = groundOf(spot, tokens);
  return contrast(resolveColor(colorOf(spot), tokens, bg), bg);
}

describe("账户胶囊与切换菜单里的第二行", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：胶囊、静止、当前、高亮、失效高亮都不低于 4.5:1", (appearance) => {
    const failing = SPOTS.map((spot) => ({ spot, ratio: Number(ratio(appearance, spot).toFixed(2)) })).filter((r) => r.ratio < 4.5);
    expect(failing).toEqual([]);
  });

  it("静止取次要文字 text-2，悬停与键盘高亮都提到 text", () => {
    expect(declarationOf(css, ".kind", "color")).toBe("var(--sk-text-2)");
    expect(declarationOf(css, ".itemLine", "color")).toBe("var(--sk-text-2)");
    expect(LIFTED.map((selector) => declarationOf(css, selector, "color"))).toEqual(["var(--sk-text)", "var(--sk-text)"]);
  });

  it("实际对比度：胶囊与菜单白底日樱 5.63、夜樱 5.73，当前行 4.80 / 4.87，高亮 13.10 / 9.87，失效高亮 5.36 / 5.64", () => {
    expect(ratio("日樱 · 樱粉", "capsule")).toBeCloseTo(5.63, 2);
    expect(ratio("夜樱 · 樱粉", "capsule")).toBeCloseTo(5.73, 2);
    expect(ratio("日樱 · 樱粉", "rest")).toBeCloseTo(5.63, 2);
    expect(ratio("夜樱 · 樱粉", "rest")).toBeCloseTo(5.73, 2);
    expect(ratio("日樱 · 樱粉", "current")).toBeCloseTo(4.8, 2);
    expect(ratio("夜樱 · 樱粉", "current")).toBeCloseTo(4.87, 2);
    expect(ratio("日樱 · 樱粉", "highlighted")).toBeCloseTo(13.1, 2);
    expect(ratio("夜樱 · 樱粉", "highlighted")).toBeCloseTo(9.87, 2);
    expect(ratio("日樱 · 樱粉", "expiredHighlighted")).toBeCloseTo(5.36, 2);
    expect(ratio("夜樱 · 樱粉", "expiredHighlighted")).toBeCloseTo(5.64, 2);
  });

  it("text-2 留在叠层上只有日樱 4.78、夜樱 4.22，所以高亮时要提", () => {
    const onOverlay = (appearance: AppearanceName) => {
      const tokens = tokensOf(appearance);
      const bg = groundOf("highlighted", tokens);
      return contrast(resolveColor("var(--sk-text-2)", tokens, bg), bg);
    };
    expect(onOverlay("日樱 · 樱粉")).toBeCloseTo(4.78, 2);
    expect(onOverlay("夜樱 · 樱粉")).toBeCloseTo(4.22, 2);
  });
});
