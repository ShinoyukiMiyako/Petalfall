// 中文的字号不小于 12px：雅黑在 11px 上笔画糊成一团。11px 的 micro 与 mono-sm 两档只放数字与西文。
// 下面这些文字会显示中文（文案、后端与读取失败的错误原文、用户选的路径、实例名、Mod 文件名、游戏日志），
// jsdom 不算样式，这里直接读样式表：每条规则落到这个元素上的字号都要解析成不小于 12 的像素，不许回到 11 的两档。

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8").replace(/\r\n/g, "\n").replace(/\/\*[\s\S]*?\*\//g, "");

const tokens = read("./tokens.css");

const MIN_CJK_PX = 12;
const SMALL_TOKENS = ["--sk-fs-micro", "--sk-fs-mono-sm"];

/** 样式表路径（相对本文件）与其中显示中文的类。 */
const CJK_TEXT: readonly (readonly [string, readonly string[]])[] = [
  ["../pages/settings/settings.module.css", [".navHint", ".errorText", ".monoLine", ".calloutPath", ".dirPath", ".dataDirPath", ".licenseText"]],
  ["../shell/AccountCapsule.module.css", [".kind"]],
  ["../ui/LoadedBody.module.css", [".errorText"]],
  ["../ui/RadioTile.module.css", [".note"]],
  ["../flows/onboarding/onboarding.module.css", [".errorText", ".extraPath"]],
  ["../flows/create-instance/create.module.css", [".stepValue", ".path"]],
  ["../pages/discover/pack.module.css", [".coverName"]],
  ["../pages/instance/instance.module.css", [".modFile", ".fileChip"]],
  ["../launch/LaunchCapsule.module.css", [".sub", ".title", ".pill"]],
  ["../shell/StatusIsland.module.css", [".logLine"]],
  ["../shell/PageError.module.css", [".detail"]],
  ["../shell/UpdatePanel.module.css", [".failureRaw"]],
  ["../setup/views/Legal.module.css", [".licenseText", ".failedDetail"]],
  ["../art/petals/PetalCanvas.gallery.module.css", [".stats"]],
  ["../dev/diagnostics.gallery.module.css", [".result"]],
];

/** 按顶层（括号外）的分隔符切开：选择器列表按逗号，复合选择器按空白与组合符。 */
function splitTopLevel(text: string, isSeparator: (ch: string) => boolean): string[] {
  const parts: string[] = [];
  let depth = 0;
  let current = "";
  for (const ch of text) {
    if (ch === "(" || ch === "[") depth += 1;
    if (ch === ")" || ch === "]") depth -= 1;
    if (depth === 0 && isSeparator(ch)) {
      if (current.trim() !== "") parts.push(current.trim());
      current = "";
      continue;
    }
    current += ch;
  }
  if (current.trim() !== "") parts.push(current.trim());
  return parts;
}

/** 选择器的主体（最右边的复合选择器）是不是这个类：.a 认 .a、.a:hover、.a[data-x]，不认 .ab。 */
function targets(selector: string, className: string): boolean {
  const compounds = splitTopLevel(selector, (ch) => /[\s>+~]/.test(ch));
  const subject = compounds[compounds.length - 1];
  return new RegExp(`${className.replace(".", "\\.")}(?![\\w-])`).test(subject);
}

/** 主体是这个类的每条规则（含分组、嵌在 @media 等块里的）里写的 font-size。 */
function fontSizes(css: string, className: string): string[] {
  const sizes: string[] = [];
  for (const rule of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selectors = splitTopLevel(rule[1].split(";").pop() as string, (ch) => ch === ",");
    if (!selectors.some((selector) => targets(selector, className))) continue;
    for (const decl of rule[2].split(";")) {
      const at = decl.indexOf(":");
      if (at >= 0 && decl.slice(0, at).trim() === "font-size") sizes.push(decl.slice(at + 1).trim());
    }
  }
  return sizes;
}

/** 字号换成像素：字面的 px，或 :root 上值为 px 的字号令牌。 */
function px(value: string): number {
  const literal = /^(\d+(?:\.\d+)?)px$/.exec(value);
  if (literal !== null) return Number(literal[1]);
  const token = /^var\((--sk-fs-[a-z0-9-]+)\)$/.exec(value);
  if (token === null) throw new Error(`认不出的字号：${value}`);
  const declared = new RegExp(`${token[1]}:\\s*(\\d+(?:\\.\\d+)?)px;`).exec(tokens);
  if (declared === null) throw new Error(`令牌 ${token[1]} 没有像素值`);
  return Number(declared[1]);
}

describe("中文最小 12px", () => {
  it("判据：11 的两档低于下限，替换用的 caption 与 mono 恰好是 12", () => {
    for (const name of SMALL_TOKENS) expect(px(`var(${name})`), name).toBe(11);
    expect(px("var(--sk-fs-caption)")).toBe(MIN_CJK_PX);
    expect(px("var(--sk-fs-mono)")).toBe(MIN_CJK_PX);
  });

  it("判据认得出分组、伪类与后代选择器里的同一个类，不认前缀相同的别的类", () => {
    const css = ".a,\n.b:hover { font-size: 11px; }\n.x .a[data-on] { font-size: 13px; }\n.ab { font-size: 9px; }\n.a .y { font-size: 9px; }";
    expect(fontSizes(css, ".a")).toEqual(["11px", "13px"]);
  });

  it.each(CJK_TEXT.flatMap(([path, classes]) => classes.map((className) => [`${path} ${className}`, path, className] as const)))(
    "%s 的字号不小于 12px，不用 micro 与 mono-sm",
    (_name, path, className) => {
      const sizes = fontSizes(read(path), className);
      expect(sizes.length, "没有一条规则给它定字号：字号会从外层继承，这条守卫就落空了").toBeGreaterThan(0);
      for (const size of sizes) {
        expect(SMALL_TOKENS.some((name) => size.includes(name)), size).toBe(false);
        expect(px(size), size).toBeGreaterThanOrEqual(MIN_CJK_PX);
      }
    },
  );
});
