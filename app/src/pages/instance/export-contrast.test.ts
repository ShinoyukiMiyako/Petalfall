// 导出整合包弹窗里的说明文字：存档行下的「地图类整合包才需要…」、停用 Mod 勾选框下的「导入后仍是停用状态」（两处同为 .extraHint，
// 落在弹窗正文上），预览统计里「走 Modrinth 索引」下的「导入时从 Modrinth 下载」（.kvHint，落在 Inset 上）。
// 它们说明勾选的后果，不是装饰也不是禁用控件里的字，不能用 text-3（日樱落在白底约 2.19:1），对各自所在的底至少 4.5:1。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../../test/contrast";

const css = readCss(new URL("./export.module.css", import.meta.url));
const modalCss = readCss(new URL("../../ui/Modal.module.css", import.meta.url));
const insetCss = readCss(new URL("../../ui/Inset.module.css", import.meta.url));

/** 弹窗正文、预览统计的 Inset 两种底。 */
type Ground = "dialog" | "panel";

function groundColor(ground: Ground, tokens: Map<string, string>): Rgb {
  const surface = resolveColor("var(--sk-surface)", tokens, [0, 0, 0]);
  if (ground === "dialog") return resolveColor(declarationOf(modalCss, ".dialog", "background"), tokens, surface);
  return resolveColor(declarationOf(insetCss, ".inset", "background"), tokens, surface);
}

function ratio(appearance: AppearanceName, selector: string, ground: Ground): number {
  const tokens = tokensOf(appearance);
  const bg = groundColor(ground, tokens);
  return contrast(resolveColor(declarationOf(css, selector, "color"), tokens, bg), bg);
}

// 中性色不随强调色变，同一明暗下三种强调色的比值相同：日樱白底 5.63、bg-2 底 4.90，夜樱 5.73、5.94
const EXPECTED: readonly [AppearanceName, number, number][] = [
  ["日樱 · 樱粉", 5.63, 4.9],
  ["日樱 · 紫水晶", 5.63, 4.9],
  ["日樱 · 青苔", 5.63, 4.9],
  ["夜樱 · 樱粉", 5.73, 5.94],
  ["夜樱 · 紫水晶", 5.73, 5.94],
  ["夜樱 · 青苔", 5.73, 5.94],
];

describe("导出整合包弹窗里的说明文字", () => {
  it("六种外观都列到了", () => {
    expect(EXPECTED.map(([appearance]) => appearance).sort()).toEqual(Object.keys(APPEARANCES).sort());
  });

  it("全部取次要文字 text-2", () => {
    expect(declarationOf(css, ".extraHint", "color")).toBe("var(--sk-text-2)");
    expect(declarationOf(css, ".kvHint", "color")).toBe("var(--sk-text-2)");
  });

  it.each(EXPECTED)("%s：勾选项说明对弹窗正文 %f、统计说明对 Inset %f，都不低于 4.5:1", (appearance, onDialog, onPanel) => {
    const extra = ratio(appearance, ".extraHint", "dialog");
    const kv = ratio(appearance, ".kvHint", "panel");
    expect(extra).toBeCloseTo(onDialog, 2);
    expect(kv).toBeCloseTo(onPanel, 2);
    expect(Math.min(extra, kv)).toBeGreaterThanOrEqual(4.5);
  });
});
