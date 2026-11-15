// 通用菜单（Menu）与下拉选择（Select）的条目：悬停与键盘高亮铺 --sk-fill-hover 叠层，叠层合成在浮层面板（Popover）的底上。
// 叠层上的标签是 text；静止为 text-2 的图标与提示（菜单右侧的快捷键或计数、下拉的第二行说明）高亮时一并提到 text，
// text-2 留在叠层上夜樱只有 4.22:1。菜单的勾、下拉已选项的标签与勾是强调色，在叠层上仍过线，保留本色；
// 危险项高亮铺 danger-soft、不铺叠层，字与图标保持危险色。文字至少 4.5:1，图标至少 3:1。
// 禁用项整体半透明，属于不可操作的控件，不计对比度。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, declarationsOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const menuCss = readCss(new URL("./Menu.module.css", import.meta.url));
const selectCss = readCss(new URL("./Select.module.css", import.meta.url));
const popoverCss = readCss(new URL("./Popover.module.css", import.meta.url));

/** 高亮时把 text-2 的图标与提示提到 text 的选择器；菜单的危险项不在其中。 */
const MENU_LIFTED = [".item[data-highlight]:not([data-danger]) .icon", ".item[data-highlight]:not([data-danger]) .hint"] as const;
const SELECT_LIFTED = [".option[data-highlight] .optionIcon", ".option[data-highlight] .optionHint"] as const;

interface Spot {
  name: string;
  css: string;
  /** 取字色的规则。 */
  color: string;
  /** 取高亮底的规则，底合成在浮层面板上。 */
  ground: string;
  min: 4.5 | 3;
}

const SPOTS: readonly Spot[] = [
  { name: "菜单标签", css: menuCss, color: ".item", ground: ".item[data-highlight]", min: 4.5 },
  { name: "菜单图标", css: menuCss, color: MENU_LIFTED[0], ground: ".item[data-highlight]", min: 3 },
  { name: "菜单提示", css: menuCss, color: MENU_LIFTED[1], ground: ".item[data-highlight]", min: 4.5 },
  { name: "菜单的勾", css: menuCss, color: ".check", ground: ".item[data-highlight]", min: 3 },
  { name: "危险项标签", css: menuCss, color: ".item[data-danger]", ground: ".item[data-danger][data-highlight]", min: 4.5 },
  { name: "危险项图标", css: menuCss, color: ".item[data-danger] .icon", ground: ".item[data-danger][data-highlight]", min: 3 },
  { name: "下拉标签", css: selectCss, color: ".option", ground: ".option[data-highlight]", min: 4.5 },
  { name: "下拉已选标签", css: selectCss, color: '.option[aria-selected="true"]', ground: ".option[data-highlight]", min: 4.5 },
  { name: "下拉图标", css: selectCss, color: SELECT_LIFTED[0], ground: ".option[data-highlight]", min: 3 },
  { name: "下拉说明", css: selectCss, color: SELECT_LIFTED[1], ground: ".option[data-highlight]", min: 4.5 },
  { name: "下拉的勾", css: selectCss, color: ".check", ground: ".option[data-highlight]", min: 3 },
];

const panelOf = (tokens: Map<string, string>): Rgb => resolveColor(declarationOf(popoverCss, ".panel", "background"), tokens, [0, 0, 0]);

function ratio(appearance: AppearanceName, spot: Spot): number {
  const tokens = tokensOf(appearance);
  const bg = resolveColor(declarationOf(spot.css, spot.ground, "background"), tokens, panelOf(tokens));
  return contrast(resolveColor(declarationOf(spot.css, spot.color, "color"), tokens, bg), bg);
}

const round = (n: number) => Number(n.toFixed(2));

/** 按 SPOTS 的顺序。中性字两套明暗各一个值；强调色随三种强调色变。 */
const PINNED: Record<AppearanceName, readonly number[]> = {
  "日樱 · 樱粉": [13.1, 13.1, 13.1, 5.26, 5.27, 5.27, 13.1, 5.26, 13.1, 13.1, 5.26],
  "日樱 · 紫水晶": [13.1, 13.1, 13.1, 5.14, 5.27, 5.27, 13.1, 5.14, 13.1, 13.1, 5.14],
  "日樱 · 青苔": [13.1, 13.1, 13.1, 5.2, 5.27, 5.27, 13.1, 5.2, 13.1, 13.1, 5.2],
  "夜樱 · 樱粉": [9.87, 9.87, 9.87, 5.42, 4.64, 4.64, 9.87, 5.42, 9.87, 9.87, 5.42],
  "夜樱 · 紫水晶": [9.87, 9.87, 9.87, 5.85, 4.64, 4.64, 9.87, 5.85, 9.87, 9.87, 5.85],
  "夜樱 · 青苔": [9.87, 9.87, 9.87, 6.84, 4.64, 4.64, 9.87, 6.84, 9.87, 9.87, 6.84],
};

describe("菜单与下拉的高亮条目", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：叠层上的文字不低于 4.5:1、图标不低于 3:1", (appearance) => {
    const failing = SPOTS.map((spot) => ({ spot: spot.name, min: spot.min, ratio: round(ratio(appearance, spot)) })).filter((r) => r.ratio < r.min);
    expect(failing).toEqual([]);
  });

  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：实际对比度", (appearance) => {
    expect(SPOTS.map((spot) => round(ratio(appearance, spot)))).toEqual(PINNED[appearance]);
  });

  it("高亮铺叠层，危险项仍铺 danger-soft；菜单与下拉的列表自己不铺底，叠层落在浮层面板上", () => {
    expect(declarationOf(menuCss, ".item[data-highlight]", "background")).toBe("var(--sk-fill-hover)");
    expect(declarationOf(selectCss, ".option[data-highlight]", "background")).toBe("var(--sk-fill-hover)");
    expect(declarationOf(menuCss, ".item[data-danger][data-highlight]", "background")).toBe("var(--sk-danger-soft)");
    expect(declarationsOf(menuCss, ".menu").has("background")).toBe(false);
    expect(declarationsOf(selectCss, ".panel").has("background")).toBe(false);
    expect(declarationsOf(selectCss, ".list").has("background")).toBe(false);
  });

  it("图标与提示静止是 text-2，高亮时提到 text；强调色与危险色保留本色", () => {
    expect([".icon", ".hint"].map((selector) => declarationOf(menuCss, selector, "color"))).toEqual(["var(--sk-text-2)", "var(--sk-text-2)"]);
    expect([".optionIcon", ".optionHint"].map((selector) => declarationOf(selectCss, selector, "color"))).toEqual(["var(--sk-text-2)", "var(--sk-text-2)"]);
    expect([...MENU_LIFTED, ...SELECT_LIFTED].map((selector) => declarationOf(selector.startsWith(".item") ? menuCss : selectCss, selector, "color"))).toEqual(
      Array(4).fill("var(--sk-text)"),
    );
    expect(declarationOf(menuCss, ".item[data-danger] .icon", "color")).toBe("var(--sk-danger)");
    expect(declarationOf(selectCss, '.option[aria-selected="true"]', "color")).toBe("var(--sk-accent-strong)");
  });

  it("text-2 留在叠层上只有日樱 4.78、夜樱 4.22，所以高亮时要提", () => {
    const onOverlay = (appearance: AppearanceName) => {
      const tokens = tokensOf(appearance);
      const bg = resolveColor("var(--sk-fill-hover)", tokens, panelOf(tokens));
      return contrast(resolveColor("var(--sk-text-2)", tokens, bg), bg);
    };
    expect(onOverlay("日樱 · 樱粉")).toBeCloseTo(4.78, 2);
    expect(onOverlay("夜樱 · 樱粉")).toBeCloseTo(4.22, 2);
  });
});
