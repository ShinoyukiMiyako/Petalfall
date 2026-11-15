// 下载快览里要被读到的两处小字：头部右侧的进行中摘要（「2 个进行中 · 最多 3 个同时进行」，没有任务时「队列空闲」），
// 与每个条目第三行的详情和状态（阶段或详情、「50% · 2.0 MB/s」、「排队中」）。它们都不是装饰也不是禁用态，不能用 text-3
// （日樱落在浮层白底上约 2.19:1），按文字 4.5:1 验收。浮层面板是实心底；条目悬停时铺 bg-2，元信息行两种底都要过。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const css = readCss(new URL("./DownloadPeek.module.css", import.meta.url));
const popoverCss = readCss(new URL("../ui/Popover.module.css", import.meta.url));

/** 浮层面板与悬停中的条目两种底。 */
type Ground = "panel" | "rowHover";

const INFO_TEXT: readonly { selector: string; grounds: readonly Ground[] }[] = [
  { selector: ".headMeta", grounds: ["panel"] },
  { selector: ".rowMeta", grounds: ["panel", "rowHover"] },
];

function groundColor(ground: Ground, tokens: Map<string, string>): Rgb {
  const panel = resolveColor(declarationOf(popoverCss, ".panel", "background"), tokens, [0, 0, 0]);
  if (ground === "panel") return panel;
  return resolveColor(declarationOf(css, ".row:hover", "background"), tokens, panel);
}

function ratio(appearance: AppearanceName, selector: string, ground: Ground): number {
  const tokens = tokensOf(appearance);
  const bg = groundColor(ground, tokens);
  return contrast(resolveColor(declarationOf(css, selector, "color"), tokens, bg), bg);
}

describe("下载快览里要被读到的小字", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：每一处对所在的底都不低于 4.5:1", (appearance) => {
    const failing = INFO_TEXT.flatMap(({ selector, grounds }) =>
      grounds.map((ground) => ({ selector, ground, ratio: Number(ratio(appearance, selector, ground).toFixed(2)) })).filter((r) => r.ratio < 4.5),
    );
    expect(failing).toEqual([]);
  });

  it("全部取次要文字 text-2", () => {
    expect(INFO_TEXT.map(({ selector }) => [selector, declarationOf(css, selector, "color")])).toEqual(INFO_TEXT.map(({ selector }) => [selector, "var(--sk-text-2)"]));
  });

  it("两种底上的实际对比度：日樱面板 5.633、悬停 4.899，夜樱 5.733、5.939", () => {
    expect(ratio("日樱 · 樱粉", ".headMeta", "panel")).toBeCloseTo(5.633, 3);
    expect(ratio("日樱 · 樱粉", ".rowMeta", "panel")).toBeCloseTo(5.633, 3);
    expect(ratio("日樱 · 樱粉", ".rowMeta", "rowHover")).toBeCloseTo(4.899, 3);
    expect(ratio("夜樱 · 樱粉", ".headMeta", "panel")).toBeCloseTo(5.733, 3);
    expect(ratio("夜樱 · 樱粉", ".rowMeta", "panel")).toBeCloseTo(5.733, 3);
    expect(ratio("夜樱 · 樱粉", ".rowMeta", "rowHover")).toBeCloseTo(5.939, 3);
  });
});
