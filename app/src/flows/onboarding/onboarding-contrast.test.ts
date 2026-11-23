// 首次启动向导左栏（bg-2）的步骤条：已跳过与还没走到的步，步名与行尾的「已跳过」是 text-2。可点回的步悬停时铺
// --sk-fill-hover 叠层，叠层合成在 bg-2 上之后 text-2 只剩日樱 4.17、夜樱 4.39，所以悬停时这两样一起提到 text。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../../test/contrast";

const css = readCss(new URL("./onboarding.module.css", import.meta.url));

const HOVER = ".railItem:hover:not(:disabled)";

/** 步名跟着整行的字色走，「已跳过」自己写颜色：两样各自静止与悬停取字色的规则。 */
const PARTS = [
  { part: "步名", rest: ".railItem", hovered: HOVER },
  { part: "已跳过", rest: ".railSkipped", hovered: `${HOVER} .railSkipped` },
] as const;

function grounds(tokens: Map<string, string>): { rest: Rgb; hovered: Rgb } {
  const side = resolveColor(declarationOf(css, ".side", "background"), tokens, [0, 0, 0]);
  return { rest: side, hovered: resolveColor(declarationOf(css, HOVER, "background"), tokens, side) };
}

function ratio(appearance: AppearanceName, selector: string, state: "rest" | "hovered"): number {
  const tokens = tokensOf(appearance);
  const bg = grounds(tokens)[state];
  return contrast(resolveColor(declarationOf(css, selector, "color"), tokens, bg), bg);
}

describe("向导步骤条上可点回的步", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：步名与「已跳过」静止、悬停都不低于 4.5:1", (appearance) => {
    const failing = PARTS.flatMap(({ part, rest, hovered }) => [
      { part, state: "rest", ratio: Number(ratio(appearance, rest, "rest").toFixed(2)) },
      { part, state: "hovered", ratio: Number(ratio(appearance, hovered, "hovered").toFixed(2)) },
    ]).filter((r) => r.ratio < 4.5);
    expect(failing).toEqual([]);
  });

  it("静止 text-2，悬停两样都提到 text", () => {
    expect(PARTS.map(({ rest }) => declarationOf(css, rest, "color"))).toEqual(["var(--sk-text-2)", "var(--sk-text-2)"]);
    expect(PARTS.map(({ hovered }) => declarationOf(css, hovered, "color"))).toEqual(["var(--sk-text)", "var(--sk-text)"]);
  });

  it("实际对比度：静止日樱 4.90、夜樱 5.94，悬停 11.43 / 10.27", () => {
    for (const { rest, hovered } of PARTS) {
      expect(ratio("日樱 · 樱粉", rest, "rest")).toBeCloseTo(4.9, 2);
      expect(ratio("夜樱 · 樱粉", rest, "rest")).toBeCloseTo(5.94, 2);
      expect(ratio("日樱 · 樱粉", hovered, "hovered")).toBeCloseTo(11.43, 2);
      expect(ratio("夜樱 · 樱粉", hovered, "hovered")).toBeCloseTo(10.27, 2);
    }
  });

  it("text-2 留在叠层上只有日樱 4.17、夜樱 4.39，所以悬停时要提", () => {
    const onOverlay = (appearance: AppearanceName) => {
      const tokens = tokensOf(appearance);
      const bg = grounds(tokens).hovered;
      return contrast(resolveColor("var(--sk-text-2)", tokens, bg), bg);
    };
    expect(onOverlay("日樱 · 樱粉")).toBeCloseTo(4.17, 2);
    expect(onOverlay("夜樱 · 樱粉")).toBeCloseTo(4.39, 2);
  });
});
