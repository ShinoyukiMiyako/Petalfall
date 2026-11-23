// 设置页分区导航卡（--sk-card 叠在窗口底色上）里每项的说明行：静止 text-2；未选项悬停铺 --sk-fill-hover 叠层，
// text-2 在叠层上夜樱只有 4.32:1，悬停时提到 text。「关于」有新版本时说明是强调色，叠层上仍够 4.5:1，不提。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../../test/contrast";

const css = readCss(new URL("./settings.module.css", import.meta.url));
const cardCss = readCss(new URL("../../ui/Card.module.css", import.meta.url));
const appCss = readCss(new URL("../../shell/AppShell.module.css", import.meta.url));

const HOVER = ".navItem:hover:not([data-on])";

/** 说明行的三种样子：静止、悬停（提到 text）、强调色说明被悬停。 */
const HINT = {
  rest: { selector: ".navHint", hovered: false },
  hovered: { selector: `${HOVER} .navHint:not([data-tone])`, hovered: true },
  accentHovered: { selector: '.navHint[data-tone="accent"]', hovered: true },
} as const;

type HintState = keyof typeof HINT;

function cardGround(tokens: Map<string, string>): Rgb {
  const app = resolveColor(declarationOf(appCss, ".app", "background"), tokens, [0, 0, 0]);
  return resolveColor(declarationOf(cardCss, ".card", "background"), tokens, app);
}

function ratio(appearance: AppearanceName, state: HintState): number {
  const tokens = tokensOf(appearance);
  const card = cardGround(tokens);
  const bg = HINT[state].hovered ? resolveColor(declarationOf(css, HOVER, "background"), tokens, card) : card;
  return contrast(resolveColor(declarationOf(css, HINT[state].selector, "color"), tokens, bg), bg);
}

describe("设置分区导航的说明行", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：静止、悬停、强调色说明被悬停都不低于 4.5:1", (appearance) => {
    const failing = (Object.keys(HINT) as HintState[]).map((state) => ({ state, ratio: Number(ratio(appearance, state).toFixed(2)) })).filter((r) => r.ratio < 4.5);
    expect(failing).toEqual([]);
  });

  it("静止 text-2，未选项悬停提到 text，强调色说明保持 accent-strong", () => {
    expect(declarationOf(css, HINT.rest.selector, "color")).toBe("var(--sk-text-2)");
    expect(declarationOf(css, HINT.hovered.selector, "color")).toBe("var(--sk-text)");
    expect(declarationOf(css, HINT.accentHovered.selector, "color")).toBe("var(--sk-accent-strong)");
  });

  it("实际对比度：静止日樱 5.59、夜樱 5.85，悬停 12.99 / 10.10，强调色说明被悬停 5.22 / 5.55", () => {
    expect(ratio("日樱 · 樱粉", "rest")).toBeCloseTo(5.59, 2);
    expect(ratio("夜樱 · 樱粉", "rest")).toBeCloseTo(5.85, 2);
    expect(ratio("日樱 · 樱粉", "hovered")).toBeCloseTo(12.99, 2);
    expect(ratio("夜樱 · 樱粉", "hovered")).toBeCloseTo(10.1, 2);
    expect(ratio("日樱 · 樱粉", "accentHovered")).toBeCloseTo(5.22, 2);
    expect(ratio("夜樱 · 樱粉", "accentHovered")).toBeCloseTo(5.55, 2);
  });

  it("text-2 留在叠层上只有日樱 4.74、夜樱 4.32，所以悬停时要提", () => {
    const onOverlay = (appearance: AppearanceName) => {
      const tokens = tokensOf(appearance);
      const bg = resolveColor(declarationOf(css, HOVER, "background"), tokens, cardGround(tokens));
      return contrast(resolveColor("var(--sk-text-2)", tokens, bg), bg);
    };
    expect(onOverlay("日樱 · 樱粉")).toBeCloseTo(4.74, 2);
    expect(onOverlay("夜樱 · 樱粉")).toBeCloseTo(4.32, 2);
  });
});
