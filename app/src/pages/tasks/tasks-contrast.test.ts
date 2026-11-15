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

// 结局角标：18 圆的实心填充上压 12 的图标。图标是认出完成、失败、已取消要看的图形，按非文字 3:1 验收，
// 对的是它自己的填充；填充是不透明色，它与底座之间另有一圈 surface 外描隔开。
const BADGE_TONES = ["ok", "danger", "muted"] as const;
type BadgeTone = (typeof BADGE_TONES)[number];

/** 每种色调在自己的规则里成对写填充与图标色，少写一样直接报错，不从 .badge 上继承。 */
function badgeDeclaration(tone: BadgeTone, property: string): string {
  return declarationOf(css, `.badge[data-tone="${tone}"]`, property);
}

function badgeRatio(appearance: AppearanceName, tone: BadgeTone): number {
  const tokens = tokensOf(appearance);
  // 角标大半压在底座上，填充万一改成半透明，按底座的底合成
  const tile = resolveColor(declarationOf(css, ".tile", "background"), tokens, cardGround(tokens));
  const fill = resolveColor(badgeDeclaration(tone, "background"), tokens, tile);
  return contrast(resolveColor(badgeDeclaration(tone, "color"), tokens, fill), fill);
}

const BADGE_CASES = (Object.keys(APPEARANCES) as AppearanceName[]).flatMap((appearance) => BADGE_TONES.map((tone) => [appearance, tone] as const));
const isNight = (appearance: AppearanceName) => appearance.startsWith("夜樱");

// 状态色实心底的两种色调：日樱的 -ink 与卡面色同为白，夜樱的 -ink 是比卡面更深的墨色
const INK_TONES = ["ok", "danger"] as const;
const INK_PINNED: Record<(typeof INK_TONES)[number], { day: number; night: number }> = {
  ok: { day: 6.3541, night: 8.5207 },
  danger: { day: 6.1951, night: 6.3908 },
};
const INK_CASES = (Object.keys(APPEARANCES) as AppearanceName[]).flatMap((appearance) => INK_TONES.map((tone) => [appearance, tone] as const));

describe("任务行的结局角标", () => {
  it("样式表里写了的角标色调正好是这里逐一验收的三种", () => {
    const declared = [...css.matchAll(/\.badge\[data-tone="([a-z]+)"\]/g)].map((m) => m[1]);
    expect([...new Set(declared)].sort()).toEqual([...BADGE_TONES].sort());
  });

  it.each(BADGE_CASES)("%s · %s：图标对填充不低于 3:1", (appearance, tone) => {
    expect(badgeRatio(appearance, tone)).toBeGreaterThanOrEqual(3);
  });

  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：已取消是次要文字 text-2 填充配 surface 图标，日樱 5.6325、夜樱 5.7334", (appearance) => {
    expect(badgeDeclaration("muted", "background")).toBe("var(--sk-text-2)");
    expect(badgeDeclaration("muted", "color")).toBe("var(--sk-surface)");
    expect(badgeRatio(appearance, "muted")).toBeCloseTo(isNight(appearance) ? 5.7334 : 5.6325, 4);
  });

  it.each(INK_CASES)("%s · %s：状态色实心底配对应的 -ink 图标，完成日樱 6.3541、夜樱 8.5207，失败日樱 6.1951、夜樱 6.3908", (appearance, tone) => {
    expect(badgeDeclaration(tone, "background")).toBe(`var(--sk-${tone})`);
    expect(badgeDeclaration(tone, "color")).toBe(`var(--sk-${tone}-ink)`);
    expect(badgeRatio(appearance, tone)).toBeCloseTo(INK_PINNED[tone][isNight(appearance) ? "night" : "day"], 4);
  });
});
