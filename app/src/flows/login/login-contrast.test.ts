// 登录弹窗里要被读到的次要文字：设备码等待行右端的「mm:ss 后过期」、登录成功胶囊里的「微软账户 · 已设为当前账户」、
// 离线账户的 UUID 标签、名字为空时的「输入用户名后生成」与下方说明、外置登录步骤条上还没走到与已走过的步骤名
// （没走到的那一步，圆点里的序号也继承这个颜色）、登录这一步服务器卡里的 API 地址、过期后转灰的设备码格。
// 它们都不是装饰也不是禁用态，不能用 text-3（日樱落在白底约 2.19:1），对各自所在的底至少 4.5:1。
// 过期的设备码作废了，但格子里的字还留在原处显示，仍要读得出：转灰是从 text 降到 text-2，不降到 text-3。
import { describe, expect, it } from "vitest";
import { APPEARANCES, contrast, declarationOf, readCss, resolveColor, tokensOf, type AppearanceName, type Rgb } from "../../test/contrast";

const css = readCss(new URL("./login.module.css", import.meta.url));
const modalCss = readCss(new URL("../../ui/Modal.module.css", import.meta.url));

/** 弹窗面板本身的底（主体不另铺底），或者文字所在的那块自带底色的容器。 */
type Ground = "dialog" | ".wait" | ".chip" | ".serverCard" | ".cell";

const INFO_TEXT: readonly { selector: string; ground: Ground }[] = [
  { selector: ".countdown", ground: ".wait" },
  { selector: ".chipLine", ground: ".chip" },
  { selector: ".uuidLabel", ground: "dialog" },
  { selector: ".uuidValue[data-empty]", ground: "dialog" },
  { selector: ".note", ground: "dialog" },
  { selector: ".stepperItem", ground: "dialog" },
  { selector: ".serverUrl", ground: ".serverCard" },
  { selector: ".code[data-expired] .cell", ground: ".cell" },
];

function groundColor(ground: Ground, tokens: Map<string, string>): Rgb {
  const surface = resolveColor("var(--sk-surface)", tokens, [0, 0, 0]);
  const dialog = resolveColor(declarationOf(modalCss, ".dialog", "background"), tokens, surface);
  if (ground === "dialog") return dialog;
  return resolveColor(declarationOf(css, ground, "background"), tokens, dialog);
}

function ratio(appearance: AppearanceName, selector: string, ground: Ground): number {
  const tokens = tokensOf(appearance);
  const bg = groundColor(ground, tokens);
  return contrast(resolveColor(declarationOf(css, selector, "color"), tokens, bg), bg);
}

const rounded = (appearance: AppearanceName) => Object.fromEntries(INFO_TEXT.map(({ selector, ground }) => [selector, Number(ratio(appearance, selector, ground).toFixed(2))]));

describe("登录弹窗里要被读到的次要文字", () => {
  it.each(Object.keys(APPEARANCES) as AppearanceName[])("%s：每一处对所在的底都不低于 4.5:1", (appearance) => {
    const failing = INFO_TEXT.map(({ selector, ground }) => ({ selector, ground, ratio: Number(ratio(appearance, selector, ground).toFixed(2)) })).filter((r) => r.ratio < 4.5);
    expect(failing).toEqual([]);
  });

  it("全部取次要文字 text-2", () => {
    expect(INFO_TEXT.map(({ selector }) => [selector, declarationOf(css, selector, "color")])).toEqual(INFO_TEXT.map(({ selector }) => [selector, "var(--sk-text-2)"]));
  });

  it("等待区与服务器卡是 bg-2 底，其余落在 surface 上：日樱 4.90 与 5.63，夜樱 5.94 与 5.73", () => {
    expect(rounded("日樱 · 樱粉")).toEqual({
      ".countdown": 4.9,
      ".chipLine": 5.63,
      ".uuidLabel": 5.63,
      ".uuidValue[data-empty]": 5.63,
      ".note": 5.63,
      ".stepperItem": 5.63,
      ".serverUrl": 4.9,
      ".code[data-expired] .cell": 5.63,
    });
    expect(rounded("夜樱 · 樱粉")).toEqual({
      ".countdown": 5.94,
      ".chipLine": 5.73,
      ".uuidLabel": 5.73,
      ".uuidValue[data-empty]": 5.73,
      ".note": 5.73,
      ".stepperItem": 5.73,
      ".serverUrl": 5.94,
      ".code[data-expired] .cell": 5.73,
    });
  });

  it("过期的设备码格仍比有效的淡一档：有效时 text，过期降到 text-2，格子的底不变", () => {
    expect(declarationOf(css, ".cell", "color")).toBe("var(--sk-text)");
    expect(declarationOf(css, ".cell", "background")).toBe("var(--sk-surface)");
    expect(() => declarationOf(css, ".code[data-expired] .cell", "background")).toThrow();
  });

  it("步骤名只有当前这一步另设颜色：已走过与没走到的都沿用 .stepperItem，上面对它的比值就覆盖了这两种状态", () => {
    const colored = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
      .filter(([, , body]) => /(^|;)\s*color\s*:/.test(body))
      .flatMap(([, selectors]) => selectors.split(",").map((s) => s.trim()))
      .filter((s) => /^\.stepperItem(\[[^\]]*\])*$/.test(s));
    expect(colored).toEqual([".stepperItem", '.stepperItem[data-state="now"]']);
  });

  it("强调色不改中性色：同一明暗下三种强调色的比值相同", () => {
    expect(rounded("日樱 · 紫水晶")).toEqual(rounded("日樱 · 樱粉"));
    expect(rounded("日樱 · 青苔")).toEqual(rounded("日樱 · 樱粉"));
    expect(rounded("夜樱 · 紫水晶")).toEqual(rounded("夜樱 · 樱粉"));
    expect(rounded("夜樱 · 青苔")).toEqual(rounded("夜樱 · 樱粉"));
  });
});
