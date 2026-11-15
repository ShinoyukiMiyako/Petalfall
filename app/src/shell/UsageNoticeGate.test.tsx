// @vitest-environment jsdom
// @module-tag heavy-page
// 使用须知的关口：首次启动与须知改版后整窗展示须知，同意才挂外壳，不同意就退出。须知文件按产物里的路径经 fetch 读，
// 确认记录走假后端（ipc/mock）的真实处理：同意之后再读一次就是「下一次启动」。
// 整个文件走真实时间：同意之后的退场要等 framer-motion 把动画播完才卸载须知、露出外壳。
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubCanvas } from "../art/illustrations/test-canvas";
import { ACTION_ERROR } from "../copy/flows";
import { USAGE_NOTICE_COPY } from "../copy/shell";
import { IpcError } from "../ipc/errors";
import { resetMocks } from "../ipc/mock";
import { acceptUsageNotice, usageNoticeAcceptance } from "../ipc/system";
import { LauncherProvider } from "../state/launcher";
import { loadUsageNoticeState, noticePending, type UsageNoticeState } from "../state/usage-notice";
import { render, stubMatchMedia, stubResizeObserver, waitForFrames, type Rendered } from "../test/dom";
import type { IpcRecorder } from "../test/fake-bridge";
import { bootOverrides, callsOf, resetRecorder } from "../test/launcher";
import { UsageNoticeGate } from "./UsageNoticeGate";
import { browserWindowPort, WindowPortProvider, type WindowPort } from "./window";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../ipc/bridge", async (orig) => (await import("../test/fake-bridge")).fakeBridge(await orig(), ipc));

const NOTICE_URL = "/usage-notice.md";
const LINK = "https://example.org/notice-repo";

let view: Rendered | undefined;
let port: WindowPort & { show: ReturnType<typeof vi.fn>; destroy: ReturnType<typeof vi.fn> };
/** 收尾动作的先后：任务记录写盘与销毁窗口各记一笔。 */
let order: string[];
/** 每个用例的随机标记，断言正文时用：显示的必须是文件里的这一份，而不是哪里写死的字。 */
let marker: string;

function randomVersion(): number {
  return 1 + Math.floor(Math.random() * 900);
}

/** 按须知的文首约定拼一份文件：标题、版本行、一段带加粗条款与链接的正文、一个小节。 */
function noticeFile(version: number): string {
  return `# 测试须知\r\n\r\n版本：${version}\r\n\r\n第一段 ${marker}。**加粗条款 ${marker}**，见 [仓库](${LINK})。\r\n\r\n## 1. 小节\r\n\r\n- 一项 ${marker}\r\n`;
}

/** 产物根目录里放着这一份须知；别的地址一律不通。 */
function serveNotice(text: string): void {
  vi.stubGlobal("fetch", async (url: string) => (url === NOTICE_URL ? new Response(text, { status: 200 }) : Promise.reject(new TypeError(`测试环境不联网：${url}`))));
}

function Shell() {
  return <p data-shell="">外壳</p>;
}

/** 真实时间下让挂起的命令与回调跑完。 */
const flush = () =>
  act(async () => {
    for (let i = 0; i < 10; i++) await new Promise<void>((resolve) => setTimeout(resolve, 0));
  });

async function mount(state: UsageNoticeState): Promise<void> {
  view = render(
    <WindowPortProvider port={port}>
      <LauncherProvider>
        <UsageNoticeGate state={state}>
          <Shell />
        </UsageNoticeGate>
      </LauncherProvider>
    </WindowPortProvider>,
  );
  await flush();
}

function unmount(): void {
  view?.unmount();
  view = undefined;
}

const screen = () => document.querySelector<HTMLElement>("[data-sk-usage-notice]");
const shellShown = () => document.querySelector("[data-shell]") !== null;

function button(label: string): HTMLButtonElement {
  const scope = screen();
  if (scope === null) throw new Error("须知页没有画出来");
  const found = [...scope.querySelectorAll<HTMLButtonElement>("footer button")].find((b) => b.textContent === label);
  if (found === undefined) throw new Error(`须知页上没有「${label}」`);
  return found;
}

function click(el: Element): void {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, detail: 1 }));
  });
}

beforeEach(() => {
  resetRecorder(ipc);
  resetMocks();
  stubResizeObserver();
  stubMatchMedia(false);
  // 标题条上的徽记是像素画，jsdom 没有画布
  stubCanvas();
  bootOverrides(ipc, { firstRun: true });
  order = [];
  ipc.overrides.set("save_task_journal", () => {
    order.push("journal");
    return null;
  });
  ipc.overrides.set("plugin:opener|open_url", () => null);
  port = {
    ...browserWindowPort(),
    show: vi.fn(async () => {}),
    destroy: vi.fn(async () => {
      order.push("destroy");
    }),
  };
  marker = Math.random().toString(36).slice(2);
});

afterEach(() => {
  unmount();
  vi.unstubAllGlobals();
});

describe("首次启动", () => {
  it("本机没有确认记录：整窗展示须知全文与版本号，加粗条款画成粗体，外壳不挂；窗口等须知画出来之后才显示", async () => {
    const version = randomVersion();
    serveNotice(noticeFile(version));
    const state = await loadUsageNoticeState();
    expect(state.acceptedVersion).toBeNull();
    expect(state.notice.version).toBe(version);
    expect(noticePending(state)).toBe(true);

    await mount(state);
    const page = screen();
    if (page === null) throw new Error("首次启动没有展示须知");
    expect(shellShown()).toBe(false);
    expect(page.querySelector("h1")?.textContent).toBe(USAGE_NOTICE_COPY.title);
    expect(page.textContent).toContain(USAGE_NOTICE_COPY.version(version));
    expect(page.textContent).toContain(USAGE_NOTICE_COPY.lead);
    expect(page.textContent).toContain(`第一段 ${marker}。`);
    expect(page.textContent).toContain(`一项 ${marker}`);
    expect([...page.querySelectorAll("strong")].map((el) => el.textContent)).toContain(`加粗条款 ${marker}`);
    // 标题行与版本行不进正文
    expect(page.textContent).not.toContain("测试须知");
    expect(page.textContent).not.toContain(`版本：${version}`);
    await waitForFrames(() => expect(port.show).toHaveBeenCalledTimes(1));
  });

  it("同意：记下这一版，须知退场之后才挂上外壳；下一次启动读到确认记录，直接进外壳、不再展示", async () => {
    const version = randomVersion();
    serveNotice(noticeFile(version));
    await mount(await loadUsageNoticeState());

    const before = Date.now();
    click(button(USAGE_NOTICE_COPY.agree));
    await flush();
    const after = Date.now();
    expect(callsOf(ipc, "accept_usage_notice")).toEqual([{ version }]);
    await waitForFrames(() => {
      expect(screen()).toBeNull();
      expect(shellShown()).toBe(true);
    });
    const recorded = await usageNoticeAcceptance();
    if (recorded === null) throw new Error("同意之后没有确认记录");
    expect(recorded.version).toBe(version);
    expect(recorded.accepted_at).toBeGreaterThanOrEqual(before);
    expect(recorded.accepted_at).toBeLessThanOrEqual(after);

    // 下一次启动
    unmount();
    const next = await loadUsageNoticeState();
    expect(next.acceptedVersion).toBe(version);
    expect(noticePending(next)).toBe(false);
    port.show.mockClear();
    await mount(next);
    expect(shellShown()).toBe(true);
    expect(screen()).toBeNull();
    expect(callsOf(ipc, "accept_usage_notice")).toHaveLength(1);
    // 不展示须知时窗口由外壳自己在首帧后显示，关口不插手
    await act(() => new Promise<void>((resolve) => setTimeout(resolve, 50)));
    expect(port.show).not.toHaveBeenCalled();
  });

  it("不同意：先把任务记录写一次，再销毁窗口退出；什么都不记，外壳不挂，下次启动照样展示", async () => {
    const version = randomVersion();
    serveNotice(noticeFile(version));
    await mount(await loadUsageNoticeState());

    click(button(USAGE_NOTICE_COPY.refuse));
    await flush();
    expect(order).toEqual(["journal", "destroy"]);
    expect(port.destroy).toHaveBeenCalledTimes(1);
    expect(callsOf(ipc, "accept_usage_notice")).toEqual([]);
    expect(shellShown()).toBe(false);
    expect(await usageNoticeAcceptance()).toBeNull();

    unmount();
    const next = await loadUsageNoticeState();
    expect(next.acceptedVersion).toBeNull();
    expect(noticePending(next)).toBe(true);
  });

  it("任务记录写不进去照样退出，原因写在页面上", async () => {
    serveNotice(noticeFile(randomVersion()));
    ipc.overrides.set("save_task_journal", () => Promise.reject(new IpcError("io", `磁盘写不进去 ${marker}`)));
    await mount(await loadUsageNoticeState());

    click(button(USAGE_NOTICE_COPY.refuse));
    await flush();
    expect(port.destroy).toHaveBeenCalledTimes(1);
    const alert = screen()?.querySelector("[role='alert']");
    expect(alert?.textContent).toContain(ACTION_ERROR.saveJournal);
    expect(alert?.textContent).toContain(`磁盘写不进去 ${marker}`);
  });

  it("记不下同意：原因写在页面上，外壳不挂，按钮回到可点；再点一次记下了就放行", async () => {
    const version = randomVersion();
    serveNotice(noticeFile(version));
    ipc.overrides.set("accept_usage_notice", () => Promise.reject(new IpcError("config_io", `配置写不进去 ${marker}`)));
    await mount(await loadUsageNoticeState());

    click(button(USAGE_NOTICE_COPY.agree));
    await flush();
    const alert = screen()?.querySelector("[role='alert']");
    expect(alert?.textContent).toContain(USAGE_NOTICE_COPY.agreeFailed);
    expect(alert?.textContent).toContain(`配置写不进去 ${marker}`);
    expect(shellShown()).toBe(false);
    expect(button(USAGE_NOTICE_COPY.agree).getAttribute("aria-disabled")).toBeNull();
    expect(button(USAGE_NOTICE_COPY.refuse).getAttribute("aria-disabled")).toBeNull();

    ipc.overrides.delete("accept_usage_notice");
    click(button(USAGE_NOTICE_COPY.agree));
    await flush();
    expect(screen()?.querySelector("[role='alert']")).toBeNull();
    await waitForFrames(() => expect(shellShown()).toBe(true));
    expect((await usageNoticeAcceptance())?.version).toBe(version);
  });

  it("同意时首次启动与否还没读到：须知留着，读到之后才挂外壳", async () => {
    let answer: (firstRun: boolean) => void = () => {
      throw new Error("is_first_run 还没被调用");
    };
    ipc.overrides.set("is_first_run", () => new Promise<boolean>((resolve) => (answer = resolve)));
    serveNotice(noticeFile(randomVersion()));
    await mount(await loadUsageNoticeState());

    click(button(USAGE_NOTICE_COPY.agree));
    await flush();
    expect(callsOf(ipc, "accept_usage_notice")).toHaveLength(1);
    await act(() => new Promise<void>((resolve) => setTimeout(resolve, 300)));
    expect(screen()).not.toBeNull();
    expect(shellShown()).toBe(false);

    answer(true);
    await flush();
    await waitForFrames(() => expect(shellShown()).toBe(true));
  });

  it("正文里的链接经打开外部网址的出口打开", async () => {
    serveNotice(noticeFile(randomVersion()));
    await mount(await loadUsageNoticeState());
    const link = screen()?.querySelector<HTMLAnchorElement>(`a[href="${LINK}"]`);
    if (link === null || link === undefined) throw new Error("正文里的链接没有画出来");
    click(link);
    await flush();
    expect(callsOf(ipc, "plugin:opener|open_url")).toEqual([{ url: LINK }]);
  });
});

describe("须知改版", () => {
  it("确认过的是上一版：重新展示，说明写的是有了新的一版；同意之后记下新的一版", async () => {
    const old = randomVersion();
    await acceptUsageNotice(old);
    serveNotice(noticeFile(old + 1));
    const state = await loadUsageNoticeState();
    expect(state.acceptedVersion).toBe(old);
    expect(noticePending(state)).toBe(true);

    await mount(state);
    const page = screen();
    if (page === null) throw new Error("须知改版后没有重新展示");
    expect(page.textContent).toContain(USAGE_NOTICE_COPY.leadUpdated);
    expect(page.textContent).not.toContain(USAGE_NOTICE_COPY.lead);
    expect(page.textContent).toContain(USAGE_NOTICE_COPY.version(old + 1));
    expect(shellShown()).toBe(false);

    click(button(USAGE_NOTICE_COPY.agree));
    await flush();
    // 第一条是上一次启动时确认的上一版
    expect(callsOf(ipc, "accept_usage_notice")).toEqual([{ version: old }, { version: old + 1 }]);
    await waitForFrames(() => expect(shellShown()).toBe(true));
    expect((await usageNoticeAcceptance())?.version).toBe(old + 1);
  });

  it("只比是不是同一版：确认过的比文件里的新（装回了旧版本）也重新展示，同一版不展示", () => {
    const version = randomVersion();
    const notice = { version, body: marker };
    expect(noticePending({ notice, acceptedVersion: null })).toBe(true);
    expect(noticePending({ notice, acceptedVersion: version - 1 })).toBe(true);
    expect(noticePending({ notice, acceptedVersion: version + 1 })).toBe(true);
    expect(noticePending({ notice, acceptedVersion: version })).toBe(false);
  });
});

describe("启动时读须知", () => {
  it("须知文件不在：照实报错，不当成这次不用展示", async () => {
    vi.stubGlobal("fetch", async () => new Response("Not Found", { status: 404 }));
    await expect(loadUsageNoticeState()).rejects.toThrow(`使用须知读不出来：${NOTICE_URL} 返回 404`);
  });

  it("文首没有版本行：照实报错，不猜一个版本号", async () => {
    serveNotice(`# 测试须知\n\n正文 ${marker}\n`);
    await expect(loadUsageNoticeState()).rejects.toThrow("使用须知的文首应是一行「# 标题」，接着一行「版本：N」");
  });

  it("确认记录读不出来：照实报错", async () => {
    serveNotice(noticeFile(randomVersion()));
    ipc.overrides.set("usage_notice_acceptance", () => Promise.reject(new IpcError("internal", `门面坏了 ${marker}`)));
    await expect(loadUsageNoticeState()).rejects.toThrow(`门面坏了 ${marker}`);
  });
});
