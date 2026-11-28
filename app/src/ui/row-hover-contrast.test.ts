// 列表行（ListRow）与表格行（Table）的悬停与按下：铺 --sk-fill-hover / --sk-fill-press 叠层，不铺实色 bg-2。
// 夜樱的 bg-2 与卡片底几乎同色，实色悬停与按它混出的悬停都看不出来；叠层半透明，先与所在的底合成，在卡片与弹窗上都拉得开，
// 按下比悬停深一档。叠层上 text-2 不到 4.5:1，列表行的说明与尾部文字在铺叠层时提到 text；表格行的字本来就是 text。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, declarationsOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const listRowCss = readCss(new URL("./ListRow.module.css", import.meta.url));
const tableCss = readCss(new URL("./Table.module.css", import.meta.url));
const shellCss = readCss(new URL("../shell/AppShell.module.css", import.meta.url));
const cardCss = readCss(new URL("./Card.module.css", import.meta.url));
const modalCss = readCss(new URL("./Modal.module.css", import.meta.url));

type Tokens = Map<string, string>;
type State = "hover" | "active";

const BLACK: Rgb = [0, 0, 0];

interface Ground {
  name: string;
  color: (tokens: Tokens) => Rgb;
}

/** 列表放在卡片与弹窗里，表格放在卡片里；两种底都核，叠层换到哪种底上都要拉得开。 */
const GROUNDS: readonly Ground[] = [
  {
    name: "卡片",
    color: (tokens) => resolveColor(declarationOf(cardCss, ".card", "background"), tokens, resolveColor(declarationOf(shellCss, ".app", "background"), tokens, BLACK)),
  },
  { name: "弹窗", color: (tokens) => resolveColor(declarationOf(modalCss, ".dialog", "background"), tokens, BLACK) },
];

interface Component {
  name: "列表行" | "表格行";
  css: string;
  /** 悬停与按下铺底的规则，按下可能有几条。 */
  fills: Record<State, readonly string[]>;
  /** 叠层上的字：静止时取色的规则与铺叠层时取色的规则。 */
  inks: readonly { rest: string; lifted: Record<State, readonly string[]> }[];
}

const TABLE_PRESS = '.row[data-activatable]:active:not(:has(:is(button, a[href], input, select, textarea, label, [role="switch"], [role="checkbox"], [role="button"]):active))';

/** 列表行铺叠层时把说明与尾部文字提到 text 的选择器（同一条规则）：悬停一条，按下按有没有标题按钮各一条。 */
const LIFTED_HOVER = ".row[data-interactive]:not([data-selected], [data-disabled]):hover :is(.meta, .trailing)";
const LIFTED_ACTIVE = [
  ".row[data-interactive]:not([data-overlay], [data-selected], [data-disabled]):active :is(.meta, .trailing)",
  ".row[data-overlay]:not([data-selected]):has(.titleButton:active) :is(.meta, .trailing)",
] as const;

const COMPONENTS: readonly Component[] = [
  {
    name: "列表行",
    css: listRowCss,
    fills: { hover: [".row[data-interactive]:hover"], active: [".row[data-interactive]:not([data-overlay], [data-disabled]):active", ".row[data-overlay]:has(.titleButton:active)"] },
    inks: [".meta", ".trailing"].map((rest) => ({ rest, lifted: { hover: [LIFTED_HOVER], active: LIFTED_ACTIVE } })),
  },
  { name: "表格行", css: tableCss, fills: { hover: [".row:hover"], active: [TABLE_PRESS] }, inks: [{ rest: ".row", lifted: { hover: [".row"], active: [".row"] } }] },
];

/** 一种状态铺的底：几条规则必须一致，不一致直接报错。 */
function fillOf(component: Component, state: State): string {
  const values = [...new Set(component.fills[state].map((selector) => declarationOf(component.css, selector, "background")))];
  if (values.length !== 1) throw new Error(`${component.name}${state} 的几条规则铺的底不一样：${values.join("、")}`);
  return values[0];
}

const round = (n: number) => Number(n.toFixed(2));
const APPEARANCE_NAMES = Object.keys(APPEARANCES) as AppearanceName[];
const modeOf = (appearance: AppearanceName) => (appearance.startsWith("日樱") ? "day" : "night");

/** 一层底铺上去之后与底本身的对比度，按 GROUNDS 的顺序。 */
function separation(appearance: AppearanceName, fill: string): number[] {
  const tokens = tokensOf(appearance);
  return GROUNDS.map((ground) => {
    const below = ground.color(tokens);
    return round(contrast(below, resolveColor(fill, tokens, below)));
  });
}

/** 叠层上某个字色的对比度，各种底里最低的那个。 */
function lowestInk(appearance: AppearanceName, fill: string, ink: string): number {
  const tokens = tokensOf(appearance);
  return round(
    Math.min(
      ...GROUNDS.map((ground) => {
        const bg = resolveColor(fill, tokens, ground.color(tokens));
        return contrast(resolveColor(ink, tokens, bg), bg);
      }),
    ),
  );
}

describe("列表行与表格行的悬停与按下叠层", () => {
  it.each(COMPONENTS)("$name：悬停铺 fill-hover、按下铺 fill-press，样式表里不再有实色 bg-2", (component) => {
    expect([fillOf(component, "hover"), fillOf(component, "active")]).toEqual(["var(--sk-fill-hover)", "var(--sk-fill-press)"]);
    expect(component.css).not.toContain("var(--sk-bg-2)");
  });

  it("贴边列表不另写悬停与按下：与内嵌列表同一套叠层", () => {
    for (const selector of ['.row[data-variant="flush"][data-interactive]:hover', '.row[data-variant="flush"][data-interactive]:not([data-overlay], [data-disabled]):active', '.row[data-variant="flush"][data-overlay]:has(.titleButton:active)']) {
      expect(() => declarationsOf(listRowCss, selector)).toThrow();
    }
  });

  it.each(APPEARANCE_NAMES)("%s：卡片与弹窗上悬停都拉得开，按下比悬停更深", (appearance) => {
    const [hover, press] = [separation(appearance, "var(--sk-fill-hover)"), separation(appearance, "var(--sk-fill-press)")];
    expect(hover).toEqual(modeOf(appearance) === "day" ? [1.18, 1.18] : [1.35, 1.36]);
    expect(press).toEqual(modeOf(appearance) === "day" ? [1.34, 1.34] : [1.66, 1.67]);
    press.forEach((ratio, i) => expect(ratio).toBeGreaterThan(hover[i]));
  });

  it("原来的实色 bg-2 在夜樱卡片上只差 1.01、弹窗上 1.04，表格按它混出的 60% 悬停在卡片上 1.01", () => {
    expect(separation("夜樱 · 樱粉", "var(--sk-bg-2)")).toEqual([1.01, 1.04]);
    expect(separation("夜樱 · 樱粉", "color-mix(in srgb, var(--sk-bg-2) 60%, transparent)")[0]).toBe(1.01);
  });
});

describe("叠层上的字", () => {
  it.each(APPEARANCE_NAMES)("%s：铺叠层时字对各种底都不低于 4.5:1", (appearance) => {
    const failing = COMPONENTS.flatMap((component) =>
      component.inks.flatMap(({ rest, lifted }) =>
        (["hover", "active"] as const).flatMap((state) =>
          lifted[state].map((selector) => ({ component: component.name, rest, state, ratio: lowestInk(appearance, fillOf(component, state), declarationOf(component.css, selector, "color")) })),
        ),
      ),
    ).filter((r) => r.ratio < 4.5);
    expect(failing).toEqual([]);
  });

  it("列表行的说明与尾部文字静止时是 text-2，铺叠层时提到 text；表格行的字一直是 text", () => {
    for (const rest of [".meta", ".trailing"]) expect(declarationOf(listRowCss, rest, "color")).toBe("var(--sk-text-2)");
    expect([LIFTED_HOVER, ...LIFTED_ACTIVE].map((selector) => declarationOf(listRowCss, selector, "color"))).toEqual(["var(--sk-text)", "var(--sk-text)", "var(--sk-text)"]);
    expect(declarationOf(tableCss, ".row", "color")).toBe("var(--sk-text)");
  });

  it("text-2 留在叠层上：悬停最低日樱 4.74、夜樱 4.22，按下 4.16、3.44，所以铺叠层时要提", () => {
    const lowest = (appearance: AppearanceName, state: State) => lowestInk(appearance, state === "hover" ? "var(--sk-fill-hover)" : "var(--sk-fill-press)", "var(--sk-text-2)");
    expect([lowest("日樱 · 樱粉", "hover"), lowest("夜樱 · 樱粉", "hover"), lowest("日樱 · 樱粉", "active"), lowest("夜樱 · 樱粉", "active")]).toEqual([4.74, 4.22, 4.16, 3.44]);
  });

  it("提到 text 之后：悬停最低日樱 12.99、夜樱 9.87，按下 11.40、8.04", () => {
    const lowest = (appearance: AppearanceName, state: State) => lowestInk(appearance, state === "hover" ? "var(--sk-fill-hover)" : "var(--sk-fill-press)", "var(--sk-text)");
    expect([lowest("日樱 · 樱粉", "hover"), lowest("夜樱 · 樱粉", "hover"), lowest("日樱 · 樱粉", "active"), lowest("夜樱 · 樱粉", "active")]).toEqual([12.99, 9.87, 11.4, 8.04]);
  });
});
