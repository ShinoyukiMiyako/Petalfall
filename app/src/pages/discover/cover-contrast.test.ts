// 整合包面板「换封面」浮层（Popover 白底）里每格下面的封面名：静止 text-2，选中格 text。悬停铺 --sk-fill-hover 叠层，
// text-2 在叠层上夜樱只有 4.22:1，悬停时提到 text。封面名自己不写颜色，跟着格子走。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, declarationsOf, readCss, resolveColor, tokensOf, type AppearanceName } from "../../test/contrast";

const css = readCss(new URL("./pack.module.css", import.meta.url));
const popoverCss = readCss(new URL("../../ui/Popover.module.css", import.meta.url));

/** 封面格的三种样子各自取字色的规则。 */
const CELL = { rest: ".coverCell", checked: '.coverCell[aria-checked="true"]', hovered: ".coverCell:hover" } as const;

type CellState = keyof typeof CELL;

function ratio(appearance: AppearanceName, state: CellState): number {
  const tokens = tokensOf(appearance);
  const panel = resolveColor(declarationOf(popoverCss, ".panel", "background"), tokens, [0, 0, 0]);
  const bg = state === "hovered" ? resolveColor(declarationOf(css, CELL.hovered, "background"), tokens, panel) : panel;
  return contrast(resolveColor(declarationOf(css, CELL[state], "color"), tokens, bg), bg);
}

describe("换封面面板的封面名", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：静止、选中、悬停都不低于 4.5:1", (appearance) => {
    const failing = (Object.keys(CELL) as CellState[]).map((state) => ({ state, ratio: Number(ratio(appearance, state).toFixed(2)) })).filter((r) => r.ratio < 4.5);
    expect(failing).toEqual([]);
  });

  it("封面名继承格子的字色：静止 text-2，悬停提到 text", () => {
    expect(declarationsOf(css, ".coverName").has("color")).toBe(false);
    expect(declarationOf(css, CELL.rest, "color")).toBe("var(--sk-text-2)");
    expect(declarationOf(css, CELL.hovered, "color")).toBe("var(--sk-text)");
  });

  it("实际对比度：静止日樱 5.63、夜樱 5.73，悬停 13.10 / 9.87", () => {
    expect(ratio("日樱 · 樱粉", "rest")).toBeCloseTo(5.63, 2);
    expect(ratio("夜樱 · 樱粉", "rest")).toBeCloseTo(5.73, 2);
    expect(ratio("日樱 · 樱粉", "hovered")).toBeCloseTo(13.1, 2);
    expect(ratio("夜樱 · 樱粉", "hovered")).toBeCloseTo(9.87, 2);
  });

  it("text-2 留在叠层上只有日樱 4.78、夜樱 4.22，所以悬停时要提", () => {
    const onOverlay = (appearance: AppearanceName) => {
      const tokens = tokensOf(appearance);
      const panel = resolveColor(declarationOf(popoverCss, ".panel", "background"), tokens, [0, 0, 0]);
      const bg = resolveColor(declarationOf(css, CELL.hovered, "background"), tokens, panel);
      return contrast(resolveColor("var(--sk-text-2)", tokens, bg), bg);
    };
    expect(onOverlay("日樱 · 樱粉")).toBeCloseTo(4.78, 2);
    expect(onOverlay("夜樱 · 樱粉")).toBeCloseTo(4.22, 2);
  });
});
