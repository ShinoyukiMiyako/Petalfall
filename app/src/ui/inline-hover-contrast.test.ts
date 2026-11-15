// 行内小图标键与行的悬停与按下：输入框清空键、标题栏 compact 档的搜索图标、下载入口、启动胶囊加载面与失败面的图标键、
// 下载快览的行。它们本身没有底，悬停铺 --sk-fill-hover、按下铺 --sk-fill-press 叠层；叠层半透明，先与所在的底合成，再与字算。
// 叠层上的图标与快览行的第三行提到 text。图标至少 3:1、文字至少 4.5:1，同色的统一按 4.5 核。
// 底按实际位置逐个列：清空键在输入框底上；标题栏两枚入口没有自己的底，落在窗口底与背景层的氛围晕染上，晕染按起始处（最浓处）
// 的不透明度叠到窗口底上，与窗口三键的守卫同一取法；胶囊图标键在加载面（进度推到右端时还叠着填充的终点色）与失败、崩溃面上；
// 快览的行在浮层面板上，行里的取消键是幽灵图标按钮，它的叠层再叠在行的叠层上。
// 下载入口打开浮层时是另一种状态，仍铺 bg-2，不在这里核。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, declarationsOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const textInputCss = readCss(new URL("./TextInput.module.css", import.meta.url));
const iconButtonCss = readCss(new URL("./IconButton.module.css", import.meta.url));
const popoverCss = readCss(new URL("./Popover.module.css", import.meta.url));
const titleBarCss = readCss(new URL("../shell/TitleBar.module.css", import.meta.url));
const peekCss = readCss(new URL("../shell/DownloadPeek.module.css", import.meta.url));
const shellCss = readCss(new URL("../shell/AppShell.module.css", import.meta.url));
const capsuleCss = readCss(new URL("../launch/LaunchCapsule.module.css", import.meta.url));

type Tokens = Map<string, string>;

interface Ground {
  name: string;
  color: (tokens: Tokens) => Rgb;
}

const BLACK: Rgb = [0, 0, 0];
const solid = (css: string, selector: string) => (tokens: Tokens) => resolveColor(declarationOf(css, selector, "background"), tokens, BLACK);

/** 氛围晕染的一团：起始色是「某色 N% 与 transparent」的径向渐变。 */
const GLOW = /radial-gradient\([^,]+, (color-mix\(in srgb, var\(--sk-[a-z0-9-]+\) \d+%, transparent\)), transparent \d+%\)/g;

/** 晕染的各团起始色；有认不出的晕染就抛错，免得漏掉一团。 */
function glowColors(tokens: Tokens): string[] {
  const ref = /^var\((--sk-[a-z0-9-]+)\)$/.exec(declarationOf(shellCss, ".backdrop", "background"));
  if (ref === null) throw new Error("背景层的底不是单个令牌");
  const ambience = tokens.get(ref[1]);
  if (ambience === undefined) throw new Error(`令牌 ${ref[1]} 没有定义`);
  const glows = [...ambience.matchAll(GLOW)].map((m) => m[1]);
  if (glows.length === 0 || glows.length !== ambience.split("radial-gradient(").length - 1) throw new Error(`认不全氛围晕染：${ambience}`);
  return glows;
}

const WINDOW = solid(shellCss, ".app");
const WINDOW_GROUNDS: readonly Ground[] = [
  { name: "窗口底", color: WINDOW },
  { name: "右上晕染最浓处", color: (tokens) => resolveColor(glowColors(tokens)[0], tokens, WINDOW(tokens)) },
  { name: "左下晕染最浓处", color: (tokens) => resolveColor(glowColors(tokens)[1], tokens, WINDOW(tokens)) },
];

const LOADING_FACE = solid(capsuleCss, '.layer[data-layer="loading"]');
const CAPSULE_GROUNDS: readonly Ground[] = [
  { name: "加载面", color: LOADING_FACE },
  // 进度填充是 accent-soft 到 accent-soft-2 的横向渐变，右缘跟着进度走；推到取消键底下时，键下是渐变的终点色
  { name: "加载面进度末端", color: (tokens) => resolveColor("var(--sk-accent-soft-2)", tokens, LOADING_FACE(tokens)) },
  { name: "失败面", color: solid(capsuleCss, '.layer[data-layer="danger"]') },
];

const PANEL = solid(popoverCss, ".panel");

interface Spot {
  name: string;
  css: string;
  /** 静止时取图标或字色的规则。 */
  rest: string;
  /** 悬停与按下：铺叠层的规则与取字色的规则；行只有悬停。 */
  states: readonly { state: "hover" | "active"; fill: string; ink: string }[];
  grounds: readonly Ground[];
}

const key = (selector: string, live = "") => [
  { state: "hover" as const, fill: `${selector}:hover${live}`, ink: `${selector}:hover${live}` },
  { state: "active" as const, fill: `${selector}:active${live}`, ink: `${selector}:active${live}` },
];

const SPOTS: readonly Spot[] = [
  { name: "清空键", css: textInputCss, rest: ".clear", states: key(".clear"), grounds: [{ name: "输入框", color: solid(textInputCss, ".box") }] },
  { name: "标题栏搜索图标", css: titleBarCss, rest: ".searchIcon", states: key(".searchIcon"), grounds: WINDOW_GROUNDS },
  { name: "下载入口", css: peekCss, rest: ".entry", states: key(".entry", ":not([data-open])"), grounds: WINDOW_GROUNDS },
  { name: "胶囊图标键", css: capsuleCss, rest: ".iconAction", states: key(".iconAction", ':not([aria-disabled="true"])'), grounds: CAPSULE_GROUNDS },
  { name: "快览行第三行", css: peekCss, rest: ".rowMeta", states: [{ state: "hover", fill: ".row:hover", ink: ".row:hover .rowMeta" }], grounds: [{ name: "浮层面板", color: PANEL }] },
];

interface Reading {
  spot: string;
  ground: string;
  state: "hover" | "active";
  ratio: number;
}

const round = (n: number) => Number(n.toFixed(2));

function readings(appearance: AppearanceName): Reading[] {
  const tokens = tokensOf(appearance);
  return SPOTS.flatMap((spot) =>
    spot.grounds.flatMap((ground) =>
      spot.states.map(({ state, fill, ink }) => {
        const bg = resolveColor(declarationOf(spot.css, fill, "background"), tokens, ground.color(tokens));
        return { spot: spot.name, ground: ground.name, state, ratio: round(contrast(resolveColor(declarationOf(spot.css, ink, "color"), tokens, bg), bg)) };
      }),
    ),
  );
}

/** 快览行悬停时，行里的取消键（幽灵图标按钮）再悬停或按下：两层叠层叠在浮层面板上。 */
const GHOST_LIVE = ':not(:disabled, [data-disabled], [aria-pressed="true"], [data-open])';

function rowCancel(appearance: AppearanceName): [number, number] {
  const tokens = tokensOf(appearance);
  const row = resolveColor(declarationOf(peekCss, ".row:hover", "background"), tokens, PANEL(tokens));
  const ratioOf = (state: "hover" | "active") => {
    const overlay = state === "hover" ? declarationOf(iconButtonCss, ".button::before", "background") : declarationOf(iconButtonCss, `.button[data-variant="ghost"]:active${GHOST_LIVE}::before`, "background");
    const bg = resolveColor(overlay, tokens, row);
    return round(contrast(resolveColor(declarationOf(iconButtonCss, `.button[data-variant="ghost"]:${state}${GHOST_LIVE}`, "color"), tokens, bg), bg));
  };
  return [ratioOf("hover"), ratioOf("active")];
}

const APPEARANCE_NAMES = Object.keys(APPEARANCES) as AppearanceName[];

/**
 * 按 SPOTS、各自的底、[悬停, 按下] 的顺序（快览行只有悬停）：清空键 2，搜索图标与下载入口各 6，胶囊图标键 6，快览行 1。
 * 晕染与进度填充随强调色变，六种外观各一组。
 */
const PINNED: Record<AppearanceName, readonly number[]> = {
  "日樱 · 樱粉": [13.1, 11.49, 12.35, 10.84, 10.48, 9.23, 11.2, 9.85, 12.35, 10.84, 10.48, 9.23, 11.2, 9.85, 13.1, 11.49, 9.8, 8.65, 11.18, 9.84, 13.1],
  "日樱 · 紫水晶": [13.1, 11.49, 12.35, 10.84, 10.23, 9.02, 11.2, 9.85, 12.35, 10.84, 10.23, 9.02, 11.2, 9.85, 13.1, 11.49, 10.09, 8.89, 11.18, 9.84, 13.1],
  "日樱 · 青苔": [13.1, 11.49, 12.35, 10.84, 10.9, 9.59, 11.2, 9.85, 12.35, 10.84, 10.9, 9.59, 11.2, 9.85, 13.1, 11.49, 10.14, 8.94, 11.18, 9.84, 13.1],
  "夜樱 · 樱粉": [9.87, 8.04, 11.38, 9.26, 8.48, 6.97, 9.7, 7.92, 11.38, 9.26, 8.48, 6.97, 9.7, 7.92, 9.87, 8.04, 7.23, 6.02, 8.44, 6.94, 9.87],
  "夜樱 · 紫水晶": [9.87, 8.04, 11.38, 9.26, 7.43, 6.15, 9.52, 7.77, 11.38, 9.26, 7.43, 6.15, 9.52, 7.77, 9.87, 8.04, 6.99, 5.82, 8.44, 6.94, 9.87],
  "夜樱 · 青苔": [9.87, 8.04, 11.38, 9.26, 7.34, 6.09, 9.31, 7.6, 11.38, 9.26, 7.34, 6.09, 9.31, 7.6, 9.87, 8.04, 6.67, 5.58, 8.44, 6.94, 9.87],
};

/** 快览行里的取消键 [悬停, 按下]，两层叠层，不随强调色变。 */
const PINNED_ROW_CANCEL: Record<"day" | "night", readonly [number, number]> = { day: [11.14, 9.8], night: [7.25, 6.02] };

const modeOf = (appearance: AppearanceName) => (appearance.startsWith("日樱") ? "day" : "night");

/** 各处用到的底，按名字去重。 */
const GROUNDS: readonly Ground[] = [...new Map(SPOTS.flatMap((spot) => spot.grounds).map((ground) => [ground.name, ground])).values()];

/** 一层填充铺上去之后与底本身的对比度：悬停底拉不拉得开。 */
function separation(appearance: AppearanceName, fill: string): Record<string, number> {
  const tokens = tokensOf(appearance);
  return Object.fromEntries(
    GROUNDS.map((ground) => {
      const below = ground.color(tokens);
      return [ground.name, round(contrast(below, resolveColor(fill, tokens, below)))];
    }),
  );
}

/** text-2 留在叠层上时，各种底里最低的那个。 */
function lowestText2(appearance: AppearanceName, fill: string): number {
  const tokens = tokensOf(appearance);
  return round(
    Math.min(
      ...GROUNDS.map((ground) => {
        const bg = resolveColor(fill, tokens, ground.color(tokens));
        return contrast(resolveColor("var(--sk-text-2)", tokens, bg), bg);
      }),
    ),
  );
}

const byMode = (mode: "day" | "night") => APPEARANCE_NAMES.filter((appearance) => modeOf(appearance) === mode);

describe("行内小图标键与行的悬停与按下叠层", () => {
  it.each(APPEARANCE_NAMES)("%s：每种底上的叠层里，图标与字不低于 4.5:1", (appearance) => {
    expect(readings(appearance).filter((r) => r.ratio < 4.5)).toEqual([]);
    expect(rowCancel(appearance).filter((ratio) => ratio < 4.5)).toEqual([]);
  });

  it.each(APPEARANCE_NAMES)("%s：实际对比度", (appearance) => {
    expect(readings(appearance).map((r) => r.ratio)).toEqual(PINNED[appearance]);
    expect(rowCancel(appearance)).toEqual(PINNED_ROW_CANCEL[modeOf(appearance)]);
  });

  it("叠层在每种底上都拉得开：悬停最少日樱 1.17、夜樱 1.33，按下 1.33、1.62；原来的实色 bg-2 在夜樱输入框、加载面与浮层面板上只差 1.04，日樱失败面 1.02", () => {
    const lowest = (mode: "day" | "night", fill: string) => Math.min(...byMode(mode).flatMap((appearance) => Object.values(separation(appearance, fill))));
    expect([lowest("day", "var(--sk-fill-hover)"), lowest("night", "var(--sk-fill-hover)")]).toEqual([1.17, 1.33]);
    expect([lowest("day", "var(--sk-fill-press)"), lowest("night", "var(--sk-fill-press)")]).toEqual([1.33, 1.62]);
    const night = separation("夜樱 · 樱粉", "var(--sk-bg-2)");
    expect([night["输入框"], night["加载面"], night["浮层面板"]]).toEqual([1.04, 1.04, 1.04]);
    expect(separation("日樱 · 樱粉", "var(--sk-bg-2)")["失败面"]).toBe(1.02);
  });

  it.each(APPEARANCE_NAMES)("%s：实色 bg-2 与窗口底、右上晕染最浓处一个深一个浅，晕染渐淡的途中总有一处与它一样亮", (appearance) => {
    const tokens = tokensOf(appearance);
    // 对纯黑的对比度随相对亮度单调增，拿它比明暗
    const lightness = (color: Rgb) => contrast(color, BLACK);
    const solid = lightness(resolveColor("var(--sk-bg-2)", tokens, BLACK));
    const [base, glow] = WINDOW_GROUNDS.slice(0, 2).map((ground) => lightness(ground.color(tokens)));
    expect(Math.sign(base - solid)).toBe(modeOf(appearance) === "day" ? 1 : -1);
    expect(Math.sign(glow - solid)).toBe(modeOf(appearance) === "day" ? -1 : 1);
  });

  it("text-2 留在叠层上：悬停最低日樱 3.57、夜樱 2.85，按下 3.15、2.38，夜樱连图标的 3:1 都不到，所以悬停与按下都提到 text", () => {
    const lowest = (mode: "day" | "night", fill: string) => Math.min(...byMode(mode).map((appearance) => lowestText2(appearance, fill)));
    expect([lowest("day", "var(--sk-fill-hover)"), lowest("night", "var(--sk-fill-hover)"), lowest("day", "var(--sk-fill-press)"), lowest("night", "var(--sk-fill-press)")]).toEqual([
      3.57, 2.85, 3.15, 2.38,
    ]);
  });
});

describe("行内小图标键与行叠层的写法", () => {
  it.each(SPOTS)("$name：静止 text-2；悬停铺 fill-hover、按下铺 fill-press，叠层上的图标与字是 text", (spot) => {
    expect(declarationOf(spot.css, spot.rest, "color")).toBe("var(--sk-text-2)");
    for (const { state, fill, ink } of spot.states) {
      expect(declarationOf(spot.css, fill, "background")).toBe(state === "hover" ? "var(--sk-fill-hover)" : "var(--sk-fill-press)");
      expect(declarationOf(spot.css, ink, "color")).toBe("var(--sk-text)");
    }
  });

  it("四枚图标键的底色与字色都随悬停时长过渡，叠层淡入淡出，不在一帧里跳", () => {
    const keys = [
      [textInputCss, ".clear"],
      [titleBarCss, ".searchIcon"],
      [peekCss, ".entry"],
      [capsuleCss, ".iconAction"],
    ] as const;
    for (const [css, selector] of keys) {
      const parts = declarationOf(css, selector, "transition")
        .split(",")
        .map((part) => part.trim());
      expect(parts).toEqual(expect.arrayContaining(["background-color var(--sk-dur-hover) var(--sk-ease)", "color var(--sk-dur-hover) var(--sk-ease)"]));
    }
  });

  it("下载入口打开浮层时仍是 bg-2 底、text 图标；悬停与按下的叠层只在没打开时铺，打开中悬停不变", () => {
    expect(Object.fromEntries(declarationsOf(peekCss, ".entry[data-open]"))).toEqual({ background: "var(--sk-bg-2)", color: "var(--sk-text)" });
    for (const bare of [".entry:hover", ".entry[data-open]:hover"]) expect(() => declarationsOf(peekCss, bare)).toThrow();
  });
});
