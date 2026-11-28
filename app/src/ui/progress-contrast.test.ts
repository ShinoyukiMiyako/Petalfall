// 进度条与进度环的灰色（muted）填充：走到哪是要读的图形，不是文字，按非文字 3:1 验收，对的是各自的轨道。
// 填充色取描边色 line-strong；text-3 是给装饰与禁用的文字色，对轨道只有日樱 1.77、夜樱 2.58，填充一眼看不出走到哪。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName } from "../test/contrast";

const barCss = readCss(new URL("./ProgressBar.module.css", import.meta.url));
const ringCss = readCss(new URL("./ProgressRing.module.css", import.meta.url));

interface Part {
  name: string;
  fill: () => string;
  track: () => string;
}

const PARTS: readonly Part[] = [
  { name: "进度条", fill: () => declarationOf(barCss, '.bar[data-tone="muted"]', "--bar-fill"), track: () => declarationOf(barCss, ".bar", "background") },
  { name: "进度环", fill: () => declarationOf(ringCss, '.fill[data-tone="muted"]', "--ring-fill"), track: () => declarationOf(ringCss, ".track", "stroke") },
];

const APPEARANCE_NAMES = Object.keys(APPEARANCES) as AppearanceName[];
const CASES = APPEARANCE_NAMES.flatMap((appearance) => PARTS.map((part) => [appearance, part] as const));

function ratio(appearance: AppearanceName, part: Part, fill = part.fill()): number {
  const tokens = tokensOf(appearance);
  // 轨道是不透明的 bg-3，下面垫什么都不影响；万一改成半透明，这里的黑底会把比值算歪，由下面钉死的轨道令牌兜住
  const track = resolveColor(part.track(), tokens, [0, 0, 0]);
  return contrast(resolveColor(fill, tokens, track), track);
}

const round = (n: number) => Number(n.toFixed(2));

describe("灰色进度填充", () => {
  it.each(CASES)("%s · $name：填充对轨道不低于 3:1", (appearance, part) => {
    expect(ratio(appearance, part)).toBeGreaterThanOrEqual(3);
  });

  it.each(PARTS)("$name：填充取 line-strong，轨道是 bg-3", (part) => {
    expect(part.fill()).toBe("var(--sk-line-strong)");
    expect(part.track()).toBe("var(--sk-bg-3)");
  });

  it("实际对比度日樱 3.07、夜樱 3.06，强调色不改中性色；text-3 只有 1.77、2.58", () => {
    for (const part of PARTS) {
      expect(APPEARANCE_NAMES.map((appearance) => round(ratio(appearance, part)))).toEqual([3.07, 3.07, 3.07, 3.06, 3.06, 3.06]);
      expect([round(ratio("日樱 · 樱粉", part, "var(--sk-text-3)")), round(ratio("夜樱 · 樱粉", part, "var(--sk-text-3)"))]).toEqual([1.77, 2.58]);
    }
  });
});
