import { describe, expect, it } from "vitest";
import { BundledFileError } from "./bundled-file";
import { parseLegalNotice } from "./legal-notice";

/** 1 到 999 之间的随机版本号：读出来的必须是文首写的那个，而不是哪里写死的值。 */
function randomVersion(): number {
  return 1 + Math.floor(Math.random() * 999);
}

describe("须知的文首约定", () => {
  it("拆出版本号与正文：正文从版本行之后开始，首尾空行去掉", () => {
    const version = randomVersion();
    const marker = Math.random().toString(36).slice(2);
    const text = `# 使用须知\n\n版本：${version}\n\n第一段 ${marker}。\n\n## 1. 小节\n\n- 一项\n`;
    expect(parseLegalNotice(text, "使用须知")).toEqual({ version, body: `第一段 ${marker}。\n\n## 1. 小节\n\n- 一项` });
  });

  // Windows 上检出的工作区是 CRLF，构建时原样写进产物
  it("CRLF 与 CR 行尾一样认，正文的行尾统一成 \\n", () => {
    const version = randomVersion();
    expect(parseLegalNotice(`# 标题\r\n\r\n版本：${version}\r\n\r\n正文一\r\n正文二\r\n`, "隐私说明")).toEqual({ version, body: "正文一\n正文二" });
    expect(parseLegalNotice(`# 标题\r版本：${version}\r正文`, "隐私说明")).toEqual({ version, body: "正文" });
  });

  it("标题与版本行紧挨着也认", () => {
    expect(parseLegalNotice("# 标题\n版本：12\n正文", "使用须知")).toEqual({ version: 12, body: "正文" });
  });

  it.each([
    ["没有版本行", "# 标题\n\n正文\n"],
    ["版本行不在标题之后", "# 标题\n\n正文\n\n版本：1\n"],
    ["版本号是 0", "# 标题\n\n版本：0\n\n正文\n"],
    ["版本号带前导零", "# 标题\n\n版本：01\n\n正文\n"],
    ["版本号不是整数", "# 标题\n\n版本：1.1\n\n正文\n"],
    ["版本行用半角冒号", "# 标题\n\n版本:1\n\n正文\n"],
    ["版本行后面还跟着字", "# 标题\n\n版本：1（草稿）\n\n正文\n"],
    ["第一行不是一级标题", "## 标题\n\n版本：1\n\n正文\n"],
    ["文首有空行", "\n# 标题\n\n版本：1\n\n正文\n"],
  ])("%s时报错，不猜一个版本号：算这份文件读不出来，整句带上文件的称呼，原因单独一份不带称呼", (_name, text) => {
    const what = Math.random() < 0.5 ? "使用须知" : "隐私说明";
    const reason = "文首应是一行「# 标题」，接着一行「版本：N」（N 是从 1 起的整数）";
    let thrown: unknown = null;
    try {
      parseLegalNotice(text, what);
    } catch (error) {
      thrown = error;
    }
    if (!(thrown instanceof BundledFileError)) throw new Error(`文首不合约定时抛的不是 BundledFileError：${String(thrown)}`);
    expect(thrown.message).toBe(`${what}读不出来：${reason}`);
    expect(thrown.reason).toBe(reason);
  });
});
