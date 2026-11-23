// 幽灵 Button 与幽灵 IconButton 的悬停与按下：铺 --sk-fill-hover / --sk-fill-press 叠层（常驻的 ::before，只过渡透明度），
// 叠层半透明，先与按钮所在的底合成，再与字算。静止为 text-2 的字与图标留在叠层上不过线，悬停与按下一并提到 text。
// 底按实际放幽灵按钮的地方逐个列：窗口底、卡片与可点卡片的悬停、凹陷区、列表行与表格行的悬停、浮层面板、弹窗与弹窗底栏、
// 通知、四种色调的提示条与横幅，另有随强调色变的表格选中行；压在卡面图片或材质上的图标按钮静止时就垫着卡面下发的表面色，
// 「开」与菜单展开中的 accent-soft 也叠在它上面。
// 按钮文字至少 4.5:1、图标至少 3:1，两者同色，按 4.5 核。危险变体、禁用、「开」与菜单展开中不铺叠层，另核写法。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, declarationsOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../test/contrast";

const buttonCss = readCss(new URL("./Button.module.css", import.meta.url));
const iconCss = readCss(new URL("./IconButton.module.css", import.meta.url));
const shellCss = readCss(new URL("../shell/AppShell.module.css", import.meta.url));
const cardCss = readCss(new URL("./Card.module.css", import.meta.url));
const insetCss = readCss(new URL("./Inset.module.css", import.meta.url));
const listRowCss = readCss(new URL("./ListRow.module.css", import.meta.url));
const tableCss = readCss(new URL("./Table.module.css", import.meta.url));
const popoverCss = readCss(new URL("./Popover.module.css", import.meta.url));
const modalCss = readCss(new URL("./Modal.module.css", import.meta.url));
const toastCss = readCss(new URL("./Toast.module.css", import.meta.url));
const calloutCss = readCss(new URL("./Callout.module.css", import.meta.url));
const bannerCss = readCss(new URL("./Banner.module.css", import.meta.url));
const coverCss = readCss(new URL("./Cover.module.css", import.meta.url));
const libraryCss = readCss(new URL("../pages/library/library.module.css", import.meta.url));

interface Kind {
  name: "Button" | "IconButton";
  css: string;
  /** 可悬停的条件：排除禁用；IconButton 另排除「开」与菜单展开中（它们的底是 accent-soft）。 */
  live: string;
}

const KINDS: readonly Kind[] = [
  { name: "Button", css: buttonCss, live: ':not(:disabled, [aria-disabled="true"])' },
  { name: "IconButton", css: iconCss, live: ':not(:disabled, [data-disabled], [aria-pressed="true"], [data-open])' },
];

type State = "hover" | "active";

/** IconButton 静止时的底：卡面下发的垫底色，不在卡面上时透明。 */
const ICON_BASE = "var(--icon-button-base, transparent)";
/** IconButton「开」与菜单展开中的底：accent-soft 的比例与垫底色混。 */
const ICON_ON = `color-mix(in srgb, var(--sk-accent) 11%, ${ICON_BASE})`;

const ghostRule =(kind: Kind, state: State, pseudo = "") => `.button[data-variant="ghost"]:${state}${kind.live}${pseudo}`;

/** 叠层的颜色：悬停用常驻层自己的底，按下换成按下那条规则的底。 */
const overlayOf = (kind: Kind, state: State) => (state === "hover" ? declarationOf(kind.css, ".button::before", "background") : declarationOf(kind.css, ghostRule(kind, "active", "::before"), "background"));

/** 一层底：从哪张样式表的哪条规则取哪个属性；local 把组件私有的色调变量换成它在色调规则里的取值。 */
interface Layer {
  css: string;
  selector: string;
  property: string;
  local?: { name: string; css: string; selector: string };
}

const layer = (css: string, selector: string, property = "background"): Layer => ({ css, selector, property });

const WINDOW = layer(shellCss, ".app");
const CARD = layer(cardCss, ".card");
const DIALOG = layer(modalCss, ".dialog");

const TONES = ["info", "ok", "warn", "danger"] as const;
const TONE_NAME = { info: "信息", ok: "成功", warn: "警示", danger: "危险" } as const;

/** 提示条与横幅的底：同一个混色公式，色调经私有变量进来；信息色调就是根规则里的默认值。 */
const toned = (css: string, root: string, variable: string, tone: (typeof TONES)[number]): Layer => ({
  ...layer(css, root),
  local: { name: variable, css, selector: tone === "info" ? root : `${root}[data-tone="${tone}"]` },
});

interface Ground {
  name: string;
  /** 自下而上，第一层必须不透明。 */
  layers: readonly Layer[];
}

const GROUNDS: readonly Ground[] = [
  { name: "窗口底", layers: [WINDOW] },
  { name: "卡片", layers: [WINDOW, CARD] },
  { name: "可点卡片悬停", layers: [WINDOW, layer(cardCss, ".card:has(.titleButton):hover")] },
  { name: "凹陷区", layers: [WINDOW, CARD, layer(insetCss, ".inset")] },
  { name: "列表行悬停", layers: [WINDOW, CARD, layer(listRowCss, ".row[data-interactive]:hover")] },
  { name: "表格行悬停", layers: [WINDOW, CARD, layer(tableCss, ".row:hover")] },
  { name: "浮层面板", layers: [layer(popoverCss, ".panel")] },
  { name: "弹窗", layers: [DIALOG] },
  { name: "弹窗底栏", layers: [DIALOG, layer(modalCss, ".footer")] },
  { name: "通知", layers: [layer(toastCss, ".toast")] },
  ...TONES.map((tone) => ({ name: `提示条 · ${TONE_NAME[tone]}`, layers: [toned(calloutCss, ".callout", "--callout-tone", tone)] })),
  ...TONES.map((tone) => ({ name: `横幅 · ${TONE_NAME[tone]}`, layers: [WINDOW, toned(bannerCss, ".banner", "--banner-tone", tone)] })),
];

/** 表格选中行：accent-soft 叠在卡片上，随强调色变。 */
const SELECTED_ROW: Ground = { name: "表格选中行", layers: [WINDOW, CARD, layer(tableCss, ".row[data-selected]")] };

function layerValue(l: Layer): string {
  const value = declarationOf(l.css, l.selector, l.property);
  if (l.local === undefined) return value;
  const replacement = declarationOf(l.local.css, l.local.selector, l.local.name);
  const ref = `var(${l.local.name})`;
  if (!value.includes(ref)) throw new Error(`${l.selector} 的 ${l.property} 没有用到 ${ref}`);
  return value.replaceAll(ref, replacement);
}

function groundOf(ground: Ground, tokens: Map<string, string>): Rgb {
  return ground.layers.reduce<Rgb>((below, l) => resolveColor(layerValue(l), tokens, below), [0, 0, 0]);
}

function ratioOn(base: Rgb, tokens: Map<string, string>, kind: Kind, state: State, color = declarationOf(kind.css, ghostRule(kind, state), "color")): number {
  const bg = resolveColor(overlayOf(kind, state), tokens, base);
  return contrast(resolveColor(color, tokens, bg), bg);
}

const ratio = (appearance: AppearanceName, ground: Ground, kind: Kind, state: State) => {
  const tokens = tokensOf(appearance);
  return ratioOn(groundOf(ground, tokens), tokens, kind, state);
};

const round = (n: number) => Number(n.toFixed(2));
const modeOf = (appearance: AppearanceName) => (appearance.startsWith("日樱") ? "day" : "night");
const APPEARANCE_NAMES = Object.keys(APPEARANCES) as AppearanceName[];

/** 按 GROUNDS 的顺序，每种底 [悬停, 按下]。中性底与状态色的底不随强调色变，两套明暗各一组。 */
const PINNED: Record<"day" | "night", readonly (readonly [number, number])[]> = {
  // 窗口底、卡片、可点卡片悬停、凹陷区、列表行悬停、表格行悬停、浮层面板、弹窗、弹窗底栏、通知，提示条四色，横幅四色
  day: [
    [12.35, 10.84], [12.99, 11.4], [13.1, 11.49], [11.43, 10.05], [11.43, 10.05], [12.04, 10.58], [13.1, 11.49], [13.1, 11.49], [12.65, 11.1], [13.1, 11.49],
    [11.87, 10.43], [11.89, 10.45], [11.88, 10.44], [11.73, 10.31], [11.87, 10.43], [11.89, 10.45], [11.88, 10.44], [11.73, 10.31],
  ],
  night: [
    [11.38, 9.26], [10.1, 8.23], [9.29, 7.59], [10.27, 8.36], [10.27, 8.36], [10.2, 8.31], [9.87, 8.04], [9.87, 8.04], [10.76, 8.75], [9.87, 8.04],
    [8.69, 7.12], [8.63, 7.08], [8.61, 7.06], [8.86, 7.26], [8.69, 7.12], [8.63, 7.08], [8.61, 7.06], [8.86, 7.26],
  ],
};

/** 表格选中行 [悬停, 按下]，随强调色变。 */
const PINNED_SELECTED: Record<AppearanceName, readonly [number, number]> = {
  "日樱 · 樱粉": [11.11, 9.78],
  "日樱 · 紫水晶": [11.29, 9.93],
  "日樱 · 青苔": [11.32, 9.96],
  "夜樱 · 樱粉": [8.53, 7.01],
  "夜樱 · 紫水晶": [8.35, 6.87],
  "夜樱 · 青苔": [8.15, 6.71],
};

describe("幽灵按钮的悬停与按下叠层", () => {
  it.each(APPEARANCE_NAMES)("%s：各种底上的叠层里，字与图标不低于 4.5:1", (appearance) => {
    const failing = KINDS.flatMap((kind) =>
      [...GROUNDS, SELECTED_ROW].flatMap((ground) =>
        (["hover", "active"] as const).map((state) => ({ kind: kind.name, ground: ground.name, state, ratio: round(ratio(appearance, ground, kind, state)) })),
      ),
    ).filter((r) => r.ratio < 4.5);
    expect(failing).toEqual([]);
  });

  it.each(APPEARANCE_NAMES)("%s：实际对比度，Button 与 IconButton 相同", (appearance) => {
    for (const kind of KINDS) {
      expect(GROUNDS.map((ground) => [round(ratio(appearance, ground, kind, "hover")), round(ratio(appearance, ground, kind, "active"))])).toEqual(PINNED[modeOf(appearance)]);
      expect([round(ratio(appearance, SELECTED_ROW, kind, "hover")), round(ratio(appearance, SELECTED_ROW, kind, "active"))]).toEqual(PINNED_SELECTED[appearance]);
    }
  });

  it("text-2 留在叠层上：悬停最低只剩日樱 4.17、夜樱 3.68，按下 3.66 / 3.02，所以悬停与按下都要提到 text", () => {
    const lowest = (appearance: AppearanceName, state: State) => {
      const tokens = tokensOf(appearance);
      return round(Math.min(...GROUNDS.map((ground) => ratioOn(groundOf(ground, tokens), tokens, KINDS[0], state, "var(--sk-text-2)"))));
    };
    expect([lowest("日樱 · 樱粉", "hover"), lowest("夜樱 · 樱粉", "hover"), lowest("日樱 · 樱粉", "active"), lowest("夜樱 · 樱粉", "active")]).toEqual([4.17, 3.68, 3.66, 3.02]);
  });

  it("悬停底在每种底上都拉得开：叠层最少日樱 1.18、夜樱 1.33；原来的实色 bg-2 落在凹陷区与行悬停上是 1.00，夜樱浮层与弹窗上 1.04", () => {
    const separation = (appearance: AppearanceName, fill: string) => {
      const tokens = tokensOf(appearance);
      return GROUNDS.map((ground) => {
        const below = groundOf(ground, tokens);
        return round(contrast(below, resolveColor(fill, tokens, below)));
      });
    };
    const overlay = overlayOf(KINDS[0], "hover");
    expect([Math.min(...separation("日樱 · 樱粉", overlay)), Math.min(...separation("夜樱 · 樱粉", overlay))]).toEqual([1.18, 1.33]);
    const solid = Object.fromEntries(GROUNDS.map((ground, i) => [ground.name, separation("夜樱 · 樱粉", "var(--sk-bg-2)")[i]]));
    expect([solid["凹陷区"], solid["列表行悬停"], solid["浮层面板"], solid["弹窗"]]).toEqual([1, 1, 1.04, 1.04]);
  });
});

describe("幽灵按钮叠层的写法", () => {
  it.each(KINDS)("$name：每个变体都挂着常驻的叠层，静止透明，只过渡透明度；按钮自成层叠上下文，叠层垫在内容之下", (kind) => {
    const base = declarationsOf(kind.css, ".button::before");
    expect(Object.fromEntries(base)).toMatchObject({
      content: '""',
      position: "absolute",
      inset: "-1px",
      "z-index": "-1",
      "border-radius": "inherit",
      background: "var(--sk-fill-hover)",
      opacity: "0",
      "pointer-events": "none",
      transition: "opacity var(--sk-dur-hover) var(--sk-ease)",
    });
    const host = declarationsOf(kind.css, ".button");
    expect([host.get("position"), host.get("isolation")]).toEqual(["relative", "isolate"]);
  });

  it.each(KINDS)("$name：幽灵变体悬停显出叠层、按下换成 fill-press，字与图标提到 text；悬停不再铺实色底", (kind) => {
    expect(declarationOf(kind.css, ghostRule(kind, "hover", "::before"), "opacity")).toBe("1");
    expect(Object.fromEntries(declarationsOf(kind.css, ghostRule(kind, "active", "::before")))).toEqual({ background: "var(--sk-fill-press)", opacity: "1" });
    for (const state of ["hover", "active"] as const) {
      const own = declarationsOf(kind.css, ghostRule(kind, state));
      expect(own.get("color")).toBe("var(--sk-text)");
      expect(own.has("background")).toBe(false);
    }
    expect(kind.css).not.toContain("var(--sk-bg-2)");
  });

  it("Button：静止的幽灵按钮透明底、text-2；主按钮与危险按钮的叠层照旧；次按钮的叠层一直透明", () => {
    expect(Object.fromEntries(declarationsOf(buttonCss, '.button[data-variant="ghost"]'))).toEqual({ background: "transparent", color: "var(--sk-text-2)" });
    const live = KINDS[0].live;
    for (const variant of ["primary", "danger"]) {
      expect(declarationOf(buttonCss, `.button[data-variant="${variant}"]:hover${live}::before`, "opacity")).toBe("1");
      expect(declarationOf(buttonCss, `.button[data-variant="${variant}"]:active${live}::before`, "background")).toBe("var(--sk-fill-press)");
    }
    expect(() => declarationsOf(buttonCss, `.button[data-variant="secondary"]:hover${live}::before`)).toThrow();
  });

  it("IconButton：「开」与菜单展开中是叠在垫底色上的 accent-soft 底、accent-strong 图标，悬停不变；危险变体悬停仍是 danger-soft，不铺叠层", () => {
    for (const selector of ['.button[aria-pressed="true"]', ".button[data-open]", '.button[aria-pressed="true"]:hover:not(:disabled)', ".button[data-open]:hover:not(:disabled)"]) {
      expect(Object.fromEntries(declarationsOf(iconCss, selector))).toEqual({ background: ICON_ON, color: "var(--sk-accent-strong)" });
    }
    expect(Object.fromEntries(declarationsOf(iconCss, '.button[data-variant="danger"]:hover:not(:disabled, [data-disabled])'))).toEqual({ background: "var(--sk-danger-soft)", color: "var(--sk-danger)" });
    expect(() => declarationsOf(iconCss, '.button[data-variant="danger"]:hover:not(:disabled, [data-disabled])::before')).toThrow();
  });
});

describe("卡面图片与材质上的图标按钮", () => {
  const MEDIA = ['.cover[data-source="remote"]', '.cover[data-source="texture"]'];
  const LIBRARY_MEDIA = '.coverArea:has(> .cover:is([data-source="texture"], [data-source="remote"]))';
  const BLACK: Rgb = [0, 0, 0];
  const WHITE: Rgb = [255, 255, 255];
  /** 卡面下发的垫底色（两种媒体形态下相同，上一条用例核对）。 */
  const mediaBase = () => declarationOf(coverCss, MEDIA[0], "--icon-button-base");
  /** 卡面上「开」与菜单展开中的底：垫底色换成卡面下发的那一个。 */
  const onMedia = () => ICON_ON.replace(ICON_BASE, mediaBase());

  it("卡面与实例库卡片的卡面区下发表面色；图标按钮静止时就垫它，幽灵变体悬停与按下不另换底；不在卡面上时是透明", () => {
    for (const selector of MEDIA) expect(declarationOf(coverCss, selector, "--icon-button-base")).toBe("var(--sk-surface)");
    expect(declarationOf(libraryCss, LIBRARY_MEDIA, "--icon-button-base")).toBe("var(--sk-surface)");
    expect(declarationOf(iconCss, ".button", "background")).toBe(ICON_BASE);
    for (const state of ["hover", "active"] as const) {
      const own = declarationsOf(iconCss, ghostRule(KINDS[1], state));
      expect([own.has("background"), own.has("background-color")]).toEqual([false, false]);
    }
  });

  // [静止, 悬停, 按下]。原先静止时没有底，text-2 直接落在压暗层与材质上，比值随图而定
  const PINNED_MEDIA: Record<"day" | "night", readonly [number, number, number]> = { day: [5.63, 13.1, 11.49], night: [5.73, 9.87, 8.04] };

  it.each(APPEARANCE_NAMES)("%s：静止时 text-2 的图标落在表面色上；悬停与按下的叠层铺在它上面，与浮层面板上的比值相同", (appearance) => {
    const tokens = tokensOf(appearance);
    const base = resolveColor(mediaBase(), tokens, BLACK);
    const rest = contrast(resolveColor(declarationOf(iconCss, ".button", "color"), tokens, base), base);
    const panel = GROUNDS.findIndex((ground) => ground.name === "浮层面板");
    const hoverActive = [round(ratioOn(base, tokens, KINDS[1], "hover")), round(ratioOn(base, tokens, KINDS[1], "active"))];
    expect(hoverActive).toEqual(PINNED[modeOf(appearance)][panel]);
    expect([round(rest), ...hoverActive]).toEqual(PINNED_MEDIA[modeOf(appearance)]);
    expect(rest).toBeGreaterThanOrEqual(4.5);
  });

  it("「开」与菜单展开中的混色比例就是 accent-soft 令牌的比例：不垫底时与令牌一字不差", () => {
    for (const appearance of APPEARANCE_NAMES) expect(ICON_ON.replace(ICON_BASE, "transparent")).toBe(tokensOf(appearance).get("--sk-accent-soft"));
  });

  /** 卡面上「开」与菜单展开中 accent-strong 图标的对比度，随强调色变。 */
  const PINNED_ON: Record<AppearanceName, number> = {
    "日樱 · 樱粉": 5.29,
    "日樱 · 紫水晶": 5.25,
    "日樱 · 青苔": 5.33,
    "夜樱 · 樱粉": 6.25,
    "夜樱 · 紫水晶": 6.62,
    "夜樱 · 青苔": 7.57,
  };

  it.each(APPEARANCE_NAMES)("%s：卡面上「开」与菜单展开中的底不透明，不随底下的图深浅变；accent-strong 图标不低于 4.5:1", (appearance) => {
    const tokens = tokensOf(appearance);
    const onBlack = resolveColor(onMedia(), tokens, BLACK);
    expect(onBlack).toEqual(resolveColor(onMedia(), tokens, WHITE));
    // 原先直接铺 accent-soft：落在黑与白的图上是两种颜色
    expect(resolveColor("var(--sk-accent-soft)", tokens, BLACK)).not.toEqual(resolveColor("var(--sk-accent-soft)", tokens, WHITE));
    const icon = contrast(resolveColor("var(--sk-accent-strong)", tokens, onBlack), onBlack);
    expect(round(icon)).toBe(PINNED_ON[appearance]);
    expect(icon).toBeGreaterThanOrEqual(4.5);
  });
});
