// 须知弹窗的尺寸约定：jsdom 不做布局，这里直接读样式表核对声明。
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./NoticeDialog.module.css", import.meta.url), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

/** 选择器恰好是 selector 的那几条规则的声明，后写的覆盖先写的；一条都没有时抛出，免得改了类名后断言落空。 */
function declarations(selector: string): Map<string, string> {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const decls = new Map<string, string>();
  let found = false;
  for (const match of css.matchAll(new RegExp(`(?:^|\\n)\\s*${escaped}\\s*\\{([^}]*)\\}`, "g"))) {
    found = true;
    for (const decl of match[1].split(";")) {
      const at = decl.indexOf(":");
      if (at < 0) continue;
      decls.set(decl.slice(0, at).trim(), decl.slice(at + 1).trim());
    }
  }
  if (!found) throw new Error(`样式表里没有 ${selector}`);
  return decls;
}

describe("须知弹窗", () => {
  it("打开就取满高：读取中与读到后同一个框，读到时不从转圈的高度撑开", () => {
    expect(declarations(".noticeDialog").get("height")).toBe("100%");
  });
});
