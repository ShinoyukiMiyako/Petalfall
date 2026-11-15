// @vitest-environment jsdom
// 弹窗里按需读取的内容读失败时的样子：读失败的原因是后端原文，被截掉的往往正是原因本身。
import { afterEach, describe, expect, it, vi } from "vitest";
import { render, type Rendered } from "../test/dom";
import { LoadedBody, type LoadedResource } from "./LoadedBody";

let view: Rendered | undefined;
afterEach(() => {
  view?.unmount();
  view = undefined;
});

const FAILED = "协议全文读不出来";
const RETRY = "重试";

/** 读失败的一份资源画出来的那条提示；每个用例的报错原文带随机状态码，显示的必须是这份错误里的原文。 */
function renderFailure(reload: () => void): { alert: Element; message: string } {
  const message = `协议全文读不出来：/LICENSE.txt 返回 ${400 + Math.floor(Math.random() * 200)}`;
  const resource: LoadedResource<string> = { status: "error", error: new Error(message), reload };
  view = render(
    <LoadedBody resource={resource} loading="正在读取协议全文…" failed={FAILED} retry={RETRY}>
      {(text: string) => <pre>{text}</pre>}
    </LoadedBody>,
  );
  const alert = view.container.querySelector('[role="alert"]');
  if (alert === null) throw new Error("读失败时没有画出读屏会播报的提示");
  return { alert, message };
}

describe("LoadedBody 读失败", () => {
  it("标题与后端原文整段显示：原文所在的文字区按实际行数显示，不截成两行", () => {
    const { alert, message } = renderFailure(vi.fn());
    const title = alert.querySelector('[class*="calloutTitle"]');
    const error = alert.querySelector('[class*="errorText"]');
    expect(title?.textContent).toBe(FAILED);
    expect(error?.textContent).toBe(message);
    const text = error?.parentElement;
    expect(text?.getAttribute("data-full")).toBe("true");
    expect(text?.contains(title ?? null)).toBe(true);
  });
});
