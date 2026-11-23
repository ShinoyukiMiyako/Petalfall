// 资源详情里要被读到的说明文字：底栏左侧说明、统计标签、来源标签、「等 n 个版本」、面板提示（可装数、版本截断、空列表、
// 没有匹配、正在计算、整合包的排队与下载）、数据包存档选择上的实例名、前置预览的组名、已满足的行与右端版本号。
// 它们都不是装饰也不是禁用态，不能用 text-3（日樱落在白底约 2.19:1），对各自所在的底至少 4.5:1。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../../../test/contrast";

const css = readCss(new URL("./detail.module.css", import.meta.url));
const modalCss = readCss(new URL("../../../ui/Modal.module.css", import.meta.url));
const insetCss = readCss(new URL("../../../ui/Inset.module.css", import.meta.url));

/** 弹窗正文、弹窗页脚、右栏面板（Inset）三种底。 */
type Ground = "dialog" | "footer" | "panel";

// 版本标签页的空提示与截断说明落在弹窗正文上，安装面板与整合包面板里的提示落在 Inset 上，所以 .panelHint 两种底都要过
const INFO_TEXT: readonly { selector: string; grounds: readonly Ground[] }[] = [
  { selector: ".footNote", grounds: ["footer"] },
  { selector: ".statLabel", grounds: ["dialog"] },
  { selector: ".sourceLabel", grounds: ["dialog"] },
  { selector: ".moreVersions", grounds: ["dialog"] },
  { selector: ".panelHint", grounds: ["dialog", "panel"] },
  { selector: ".worldLabel", grounds: ["panel"] },
  { selector: ".previewFor", grounds: ["panel"] },
  { selector: ".previewRow[data-satisfied]", grounds: ["panel"] },
  { selector: ".previewVersion", grounds: ["panel"] },
];

function groundColor(ground: Ground, tokens: Map<string, string>): Rgb {
  const surface = resolveColor("var(--sk-surface)", tokens, [0, 0, 0]);
  if (ground === "dialog") return resolveColor(declarationOf(modalCss, ".dialog", "background"), tokens, surface);
  if (ground === "footer") return resolveColor(declarationOf(modalCss, ".footer", "background"), tokens, surface);
  return resolveColor(declarationOf(insetCss, ".inset", "background"), tokens, surface);
}

function ratio(appearance: AppearanceName, selector: string, ground: Ground): number {
  const tokens = tokensOf(appearance);
  const bg = groundColor(ground, tokens);
  return contrast(resolveColor(declarationOf(css, selector, "color"), tokens, bg), bg);
}

describe("资源详情里要被读到的说明文字", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：每一处对所在的底都不低于 4.5:1", (appearance) => {
    const failing = INFO_TEXT.flatMap(({ selector, grounds }) =>
      grounds.map((ground) => ({ selector, ground, ratio: Number(ratio(appearance, selector, ground).toFixed(2)) })).filter((r) => r.ratio < 4.5),
    );
    expect(failing).toEqual([]);
  });

  it("全部取次要文字 text-2", () => {
    expect(INFO_TEXT.map(({ selector }) => [selector, declarationOf(css, selector, "color")])).toEqual(INFO_TEXT.map(({ selector }) => [selector, "var(--sk-text-2)"]));
  });

  it("三种底上的实际对比度：日樱页脚 5.43、正文 5.63、面板 4.90，夜樱 6.18、5.73、5.94", () => {
    expect(ratio("日樱 · 樱粉", ".footNote", "footer")).toBeCloseTo(5.43, 2);
    expect(ratio("日樱 · 樱粉", ".statLabel", "dialog")).toBeCloseTo(5.63, 2);
    expect(ratio("日樱 · 樱粉", ".panelHint", "panel")).toBeCloseTo(4.9, 2);
    expect(ratio("夜樱 · 樱粉", ".footNote", "footer")).toBeCloseTo(6.18, 2);
    expect(ratio("夜樱 · 樱粉", ".statLabel", "dialog")).toBeCloseTo(5.73, 2);
    expect(ratio("夜樱 · 樱粉", ".panelHint", "panel")).toBeCloseTo(5.94, 2);
  });
});
