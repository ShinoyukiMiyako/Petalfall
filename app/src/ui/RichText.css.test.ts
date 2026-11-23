// 富文本列表的样式约定：jsdom 不做布局、不带浏览器默认样式，这里直接读样式表核对声明。
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8").replace(/\r\n/g, "\n").replace(/\/\*[\s\S]*?\*\//g, "");

const css = read("./RichText.module.css");

function rule(selector: string): Map<string, string> {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = new RegExp(`(?:^|\\n)${escaped}\\s*\\{([^}]*)\\}`).exec(css);
  if (m === null) throw new Error(`没有 ${selector} 规则`);
  const decls = new Map<string, string>();
  for (const decl of m[1].split(";")) {
    const at = decl.indexOf(":");
    if (at < 0) continue;
    decls.set(decl.slice(0, at).trim(), decl.slice(at + 1).trim());
  }
  return decls;
}

/** 间距令牌在 tokens.css 里的像素值。 */
function spacePx(value: string | undefined): number {
  if (value === undefined) throw new Error("没有写这项间距");
  const m = /^var\((--sk-space-\d+)\)$/.exec(value);
  if (m === null) throw new Error(`不是间距令牌：${value}`);
  const decl = new RegExp(`${m[1]}:\\s*(\\d+)px;`).exec(read("../styles/tokens.css"));
  if (decl === null) throw new Error(`tokens.css 里没有 ${m[1]}`);
  return Number(decl[1]);
}

/** Chromium 里正文字体（14px）下「10. 」的宽度：::marker 默认用等宽数字。 */
const TWO_DIGIT_MARKER_PX = 23.4;

describe("富文本列表的项目符号", () => {
  it("退回浏览器默认的符号：Tailwind 预检把 ol、ul 清成 none，富文本里无序要圆点、有序要数字，嵌套的逐层换符号", () => {
    expect(rule(".list").get("list-style-type")).toBe("revert");
  });

  it("有序列表的缩进放得下两位数序号，序号不伸出正文左缘；无序的圆点窄，沿用 20", () => {
    expect(spacePx(rule("ol.list").get("padding-left"))).toBeGreaterThan(TWO_DIGIT_MARKER_PX);
    expect(spacePx(rule(".list").get("padding-left"))).toBe(20);
  });

  it("只在富文本范围内恢复：全局基底不碰列表符号，界面里拿列表当容器的地方照旧不画", () => {
    expect(read("../styles/base.css")).not.toMatch(/list-style/);
    expect(read("../styles/theme.css")).not.toMatch(/list-style/);
  });
});
