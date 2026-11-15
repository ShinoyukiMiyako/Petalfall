// 标题栏搜索入口（regular、wide 档的伪输入框）：放大镜与占位文字继承入口的字色，占位文字要被读到，不能用 text-3
// （日樱落在白底约 2.19:1），静止取 text-2；悬停照旧比静止提一级，提到 text。底是入口自己的实心底。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName } from "../test/contrast";

const css = readCss(new URL("./TitleBar.module.css", import.meta.url));

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
