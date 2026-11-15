// 并行通道空出时虚线框里的「空闲」：交代这条通道眼下没有任务，是要被读到的状态，不是装饰也不是禁用态，不能用 text-3
// （日樱落在卡片上约 2.17:1），按文字 4.5:1 验收。虚线框没有自己的底，字落在并行通道卡上。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../../test/contrast";

const css = readCss(new URL("./tasks.module.css", import.meta.url));
const cardCss = readCss(new URL("../../ui/Card.module.css", import.meta.url));

/** 卡片底 --sk-card 是 rgba() 半透明色，按它的不透明度叠到窗口底 --sk-bg 上（与规范核验卡片底的口径一致）。 */
function cardGround(tokens: Map<string, string>): Rgb {
  const declared = declarationOf(cardCss, ".card", "background");
  if (declared !== "var(--sk-card)") throw new Error(`卡片底改成了 ${declared}，这里的合成要跟着改`);
  const value = tokens.get("--sk-card");
  if (value === undefined) throw new Error("令牌 --sk-card 没有定义");
  const m = /^rgba\((\d+), (\d+), (\d+), ([\d.]+)\)$/.exec(value);
  if (m === null) throw new Error(`--sk-card 不是 rgba() 色值：${value}`);
  const alpha = Number(m[4]);
  const bg = resolveColor("var(--sk-bg)", tokens, [0, 0, 0]);
  return [m[1], m[2], m[3]].map((c, i) => Number(c) * alpha + bg[i] * (1 - alpha)) as unknown as Rgb;
}

function idleRatio(appearance: AppearanceName): number {
  const tokens = tokensOf(appearance);
  const ground = cardGround(tokens);
  return contrast(resolveColor(declarationOf(css, ".laneIdle", "color"), tokens, ground), ground);
}

describe("并行通道里空闲通道的「空闲」", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：对卡片底不低于 4.5:1", (appearance) => {
    expect(idleRatio(appearance)).toBeGreaterThanOrEqual(4.5);
  });

  it("取次要文字 text-2：日樱 5.586、夜樱 5.854", () => {
    expect(declarationOf(css, ".laneIdle", "color")).toBe("var(--sk-text-2)");
    expect(idleRatio("日樱 · 樱粉")).toBeCloseTo(5.586, 3);
    expect(idleRatio("夜樱 · 樱粉")).toBeCloseTo(5.854, 3);
  });
});
