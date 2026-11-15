// jsdom 不做布局也不跑 CSS 过渡，这里直接读样式表核对窗口骨架里和动效有关的声明。
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const stripComments = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, "");
const SETUP = stripComments(readFileSync(new URL("./setup.module.css", import.meta.url), "utf8"));
const TITLE_BAR = stripComments(readFileSync(new URL("../shell/TitleBar.module.css", import.meta.url), "utf8"));
const TOKENS = readFileSync(new URL("../styles/tokens.css", import.meta.url), "utf8");

function decls(selector: string): Map<string, string> {
  return declsIn(SETUP, selector);
}

function declsIn(css: string, selector: string): Map<string, string> {
  const escaped = selector.replace(/[.[\]]/g, "\\$&");
  const match = new RegExp(`(?:^|\\n)${escaped}\\s*\\{([^}]*)\\}`).exec(css);
  if (match === null) throw new Error(`样式表里没有 ${selector}`);
  const found = new Map<string, string>();
  for (const decl of match[1].split(";")) {
    const at = decl.indexOf(":");
    if (at < 0) continue;
    found.set(decl.slice(0, at).trim(), decl.slice(at + 1).trim().replace(/\s+/g, " "));
  }
  return found;
}

describe("换步的页", () => {
  it("撑满主列，也是页里退场块的定位基准", () => {
    const pane = decls(".pane");
    expect(pane.get("position")).toBe("relative");
    expect(pane.get("flex")).toBe("1");
    expect(pane.get("flex-direction")).toBe("column");
    expect(pane.get("min-height")).toBe("0");
  });

  it("窗口按钮抬在页上面：页是定位元素、入场时带 transform，不抬就被它盖住、点不到", () => {
    expect(decls(".windowControls").get("z-index")).toBe("var(--sk-z-page)");
  });
});

describe("说明区占位", () => {
  it("两段说明叠在同一格里，格子按较高的那段撑开，占位的那段看不见", () => {
    expect(decls(".leadSlot").get("display")).toBe("grid");
    expect(decls(".leadSlot > .lead").get("grid-area")).toBe("1 / 1");
    expect(decls(".lead[data-floor]").get("visibility")).toBe("hidden");
  });
});

describe("窗口按钮", () => {
  it("底块只过渡底色和字色，焦点环（box-shadow）立刻出现、立刻消失", () => {
    const transition = decls(".winFace").get("transition");
    expect(transition?.split(",").map((part) => part.trim().split(" ")[0])).toEqual(["background-color", "color"]);
  });

  it("两个圆之间、关闭键的圆到窗口右缘与上缘都是 8px，焦点环不被窗口边缘切掉", () => {
    expect(decls(".windowControls").get("top")).toBe("0");
    expect(decls(".windowControls").get("right")).toBe("0");
    // 关闭键前面另加空隙、或圆在命中区里居中，两个圆就不等距
    expect(decls(".winButton[data-close]").has("margin-left")).toBe(false);
    expect(decls(".winButton[data-close]").get("justify-items")).toBe("start");
    const button = length(".winButton", "width");
    const close = length(".winButton[data-close]", "width");
    const face = length(".winFace", "width");
    const inset = length(".winButton[data-close] .winFace", "margin-left");
    const gap = token("space-8");
    expect((button - face) / 2 + inset).toBe(gap);
    expect(close - inset - face).toBe(gap);
    const top = (length(".winButton", "height") - length(".winFace", "height")) / 2;
    expect(top).toBe(gap);
    expect(top).toBeGreaterThanOrEqual(focusRingSpread());
  });

  it("标题与说明让开整排窗口键的命中区", () => {
    const padding = decls(".main").get("padding")?.split(" ");
    if (padding?.length !== 3) throw new Error(`.main 的内边距应写成上、左右、下三段，实得 ${padding?.join(" ")}`);
    const clearance = pixels(padding[1], ".main 的左右内边距") + length(".head", "padding-right");
    expect(clearance).toBeGreaterThanOrEqual(length(".winButton", "width") + length(".winButton[data-close]", "width"));
  });

  it("可见底色是正圆，尺寸与命中区的宽度都和主窗口标题栏的一样", () => {
    expect(length(".winFace", "height")).toBe(length(".winFace", "width"));
    expect(decls(".winFace").get("border-radius")).toBe("var(--sk-r-pill)");
    const shared: readonly (readonly [string, string])[] = [
      [".winButton", "width"],
      [".winButton[data-close]", "width"],
      [".winButton[data-close]", "justify-items"],
      [".winButton[data-close] .winFace", "margin-left"],
      [".winFace", "width"],
      [".winFace", "height"],
      [".winFace", "border-radius"],
    ];
    for (const [selector, property] of shared) {
      expect(decls(selector).get(property), `${selector} ${property}`).toBe(declsIn(TITLE_BAR, selector).get(property));
    }
  });

  it("关闭键悬停只染浅红，按下才转正红；悬停与按下的取色都和主窗口标题栏相同", () => {
    const hover = decls(".winButton[data-close]:hover .winFace");
    expect(hover.get("background")).toBe("var(--sk-danger-soft)");
    expect(hover.get("color")).toBe("var(--sk-danger)");
    const active = decls(".winButton[data-close]:active .winFace");
    expect(active.get("background")).toBe("var(--sk-win-close)");
    expect(active.get("color")).toBe("var(--sk-win-close-ink)");
    for (const selector of [".winButton:hover .winFace", ".winButton:active .winFace", ".winButton[data-close]:hover .winFace", ".winButton[data-close]:active .winFace"]) {
      expect(Object.fromEntries(decls(selector)), selector).toEqual(Object.fromEntries(declsIn(TITLE_BAR, selector)));
    }
  });
});

function token(name: string): number {
  const m = new RegExp(`--sk-${name}:\\s*(\\d+)px;`).exec(TOKENS);
  if (m === null) throw new Error(`tokens.css 里没有 --sk-${name}`);
  return Number(m[1]);
}

/** 规则里写成 Npx 或 var(--sk-space-N) 的长度。 */
function length(selector: string, property: string): number {
  const value = decls(selector).get(property);
  if (value === undefined) throw new Error(`${selector} 没有写 ${property}`);
  return pixels(value, `${selector} 的 ${property}`);
}

function pixels(value: string, where: string): number {
  const px = /^(\d+)px$/.exec(value);
  if (px !== null) return Number(px[1]);
  const space = /^var\(--sk-(space-\d+)\)$/.exec(value);
  if (space !== null) return token(space[1]);
  throw new Error(`${where} 是 ${value}，不是像素或间距令牌`);
}

/** 焦点环在底块外扩出的最大距离：--sk-focus 里各层 spread 的最大值。 */
function focusRingSpread(): number {
  const ring = /--sk-focus:\s*([^;]+);/.exec(TOKENS)?.[1];
  if (ring === undefined) throw new Error("tokens.css 里没有 --sk-focus");
  const spreads = ring.split(",").map((layer) => {
    const m = /^\s*0 0 0 (\d+)px\b/.exec(layer);
    if (m === null) throw new Error(`--sk-focus 的一层不是只带 spread 的环：${layer}`);
    return Number(m[1]);
  });
  return Math.max(...spreads);
}
