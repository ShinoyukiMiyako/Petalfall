// @vitest-environment jsdom
// 弹窗里按需读取的内容读失败时的样子：读失败的原因是后端原文，被截掉的往往正是原因本身。
import { act } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BundledFileError } from "../lib/bundled-file";
import { render, type Rendered } from "../test/dom";
import { LoadedBody, type LoadedResource } from "./LoadedBody";

let view: Rendered | undefined;
afterEach(() => {
  view?.unmount();
  view = undefined;
});

const FAILED = "协议全文读不出来";
const RETRY = "重试";

/** 随机状态码的「文件不在」：显示的必须是这份错误里的原因。 */
function missingFile(): { error: BundledFileError; reason: string } {
  const reason = `/LICENSE.txt 返回 ${400 + Math.floor(Math.random() * 200)}`;
  return { error: new BundledFileError("协议全文", reason), reason };
}

/** 读失败的一份资源画出来的那条提示。 */
function renderFailure(reload: () => void, error: Error = missingFile().error): { alert: Element } {
  const resource: LoadedResource<string> = { status: "error", error, reload };
  view = render(
    <LoadedBody resource={resource} loading="正在读取协议全文…" failed={FAILED} retry={RETRY}>
      {(text: string) => <pre>{text}</pre>}
    </LoadedBody>,
  );
  const alert = view.container.querySelector('[role="alert"]');
  if (alert === null) throw new Error("读失败时没有画出读屏会播报的提示");
  return { alert };
}

describe("LoadedBody 读失败", () => {
  it("文件读不出来：标题点名是哪份，原文只给原因、不把标题再说一遍；原文所在的文字区按实际行数显示，不截成两行", () => {
    const { error: failure, reason } = missingFile();
    const { alert } = renderFailure(vi.fn(), failure);
    const title = alert.querySelector('[class*="calloutTitle"]');
    const error = alert.querySelector('[class*="errorText"]');
    expect(title?.textContent).toBe(FAILED);
    expect(error?.textContent).toBe(reason);
    // 日志与启动错误页用的整句仍带称呼
    expect(failure.message).toBe(`${FAILED}：${reason}`);
    const text = error?.parentElement;
    expect(text?.getAttribute("data-full")).toBe("true");
    expect(text?.contains(title ?? null)).toBe(true);
  });

  it("别的错误（请求本身失败）原文里没有称呼，照原文显示", () => {
    const message = `Failed to fetch ${Math.random().toString(36).slice(2)}`;
    const { alert } = renderFailure(vi.fn(), new TypeError(message));
    expect(alert.querySelector('[class*="calloutTitle"]')?.textContent).toBe(FAILED);
    expect(alert.querySelector('[class*="errorText"]')?.textContent).toBe(message);
  });

  it("重读按钮是说明块里通行的幽灵小按钮，提示里只有这一个；点了重读一次", () => {
    const reload = vi.fn();
    const { alert } = renderFailure(reload);
    const buttons = alert.querySelectorAll("button");
    expect(buttons).toHaveLength(1);
    const [button] = buttons;
    expect(button.textContent).toBe(RETRY);
    expect(button.dataset.variant).toBe("ghost");
    expect(button.dataset.size).toBe("sm");
    act(() => {
      button.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(reload).toHaveBeenCalledTimes(1);
  });
});
