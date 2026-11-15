// 新建实例按钮只有一个加号：图标就是按钮的全部内容，按非文字 3:1 验收，底是侧栏胶囊。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName } from "../test/contrast";

const css = readCss(new URL("./Sidebar.module.css", import.meta.url));

function plusRatio(appearance: AppearanceName): number {
  const tokens = tokensOf(appearance);
  const surface = resolveColor("var(--sk-surface)", tokens, [0, 0, 0]);
  const capsule = resolveColor(declarationOf(css, ".capsule", "background"), tokens, surface);
  return contrast(resolveColor(declarationOf(css, ".create", "color"), tokens, capsule), capsule);
}

describe("侧栏新建实例按钮的加号", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：对胶囊底不低于 3:1", (appearance) => {
    expect(plusRatio(appearance)).toBeGreaterThanOrEqual(3);
  });

  it("沿用条目共用的 text-2：日樱 5.63、夜樱 5.73", () => {
    expect(declarationOf(css, ".create", "color")).toBe("var(--sk-text-2)");
    expect(plusRatio("日樱 · 樱粉")).toBeCloseTo(5.63, 2);
    expect(plusRatio("夜樱 · 樱粉")).toBeCloseTo(5.73, 2);
  });
});
