// @vitest-environment jsdom
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConfigDto } from "../../ipc/config";
import { mockInvoke } from "../../ipc/mock";
import { MOCK_DOWNLOADS_DIR, reset as resetModpackMock, seedMockDownloads } from "../../ipc/mock/handlers/modpack";
import { APP_NAME } from "../../lib/app-info";
import { OverlayProvider, OverlayRoot } from "../../shell/overlay";
import { LauncherProvider } from "../../state/launcher";
import type { BlockedFileDto } from "../../state/store";
import { drainFrames, render, stubResizeObserver, type Rendered } from "../../test/dom";
import type { IpcRecorder } from "../../test/fake-bridge";
import { bootOverrides, callsOf, configDto, deferred, resetRecorder, settle } from "../../test/launcher";
import modalStyles from "../../ui/Modal.module.css";
import { BlockedFilesModal, Fade, type BlockedOrigin } from "./BlockedFilesModal";
import styles from "./pack.module.css";
import { PickFolderProvider } from "./platform";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../../ipc/bridge", async (orig) => (await import("../../test/fake-bridge")).fakeBridge(await orig(), ipc));

let view: Rendered | undefined;

afterEach(() => {
  view?.unmount();
  view = undefined;
  vi.restoreAllMocks();
});

/**
 * jsdom 不做布局，popLayout 量退场项时读到的宽高是空的，就不会把它抽出布局。给装着探针的那一层一个非零的宽高，
 * 退场项能不能被抽出来就只取决于 ref 有没有接到这一层。
 */
function sizeProbedBoxes() {
  const real = window.getComputedStyle.bind(window);
  vi.spyOn(window, "getComputedStyle").mockImplementation((el, pseudo) => {
    const style = real(el, pseudo);
    if (!(el instanceof HTMLElement) || el.firstElementChild?.hasAttribute("data-probe") !== true) return style;
    return { width: "60px", height: "20px", direction: "ltr", getPropertyValue: (name: string) => style.getPropertyValue(name) } as unknown as CSSStyleDeclaration;
  });
}

const box = (text: string) => {
  const probe = view?.container.querySelector(`[data-probe="${text}"]`);
  if (probe === null || probe === undefined) throw new Error(`没有 ${text}`);
  const el = probe.parentElement;
  if (el === null) throw new Error(`${text} 没有外层`);
  return el;
};

describe("清单的状态淡入淡出", () => {
  it.each([
    ["行内的标签", false],
    ["说明块", true],
  ])("%s换内容：旧的一层被抽出布局（ref 接到了这一层）、不可交互，标上 data-exiting；新的一层照常在流里", (_name, block) => {
    sizeProbedBoxes();
    const tree = (id: string) => (
      <Fade id={id} block={block}>
        <b data-probe={id}>{id}</b>
      </Fade>
    );
    view = render(tree("missing"));
    expect(box("missing").hasAttribute("data-exiting")).toBe(false);

    view.rerender(tree("found"));
    const old = box("missing");
    expect(old.dataset.motionPopId).toBeDefined();
    expect(old.hasAttribute("inert")).toBe(true);
    expect(old.hasAttribute("data-exiting")).toBe(true);
    expect(box("found").dataset.motionPopId).toBeUndefined();
    expect(box("found").hasAttribute("inert")).toBe(false);
  });

  it("挂载时就在的不播入场；之后出现的从透明淡入", () => {
    const tree = (id: string | null) => (
      <Fade id={id}>
        <b data-probe={id === null ? "" : id}>{id}</b>
      </Fade>
    );
    view = render(tree("open"));
    expect(box("open").style.opacity).toBe("1");
    view.rerender(tree("found"));
    expect(box("found").style.opacity).toBe("0");
  });
});

describe("清单上关于「自动下载受限的 CurseForge 文件」的提示", () => {
  const blockedFile = (name: string, id: number): BlockedFileDto => ({
    project_id: 9,
    file_id: id,
    file_name: name,
    web_url: `https://www.curseforge.com/minecraft/mc-mods/locked/files/${id}`,
    destination: `mods/${name}`,
    sha1: "a".repeat(40),
    fingerprint: 0,
    size: 3,
  });
  const FILES = [blockedFile("locked.jar", 90), blockedFile("dep.jar", 91)];
  const INTRO = "这 2 个文件的作者不允许启动器直接下载。到 CurseForge 网页下载后，回到这里重新检查。";
  const OFF = `在「设置 → 下载」里打开「自动下载受限的 CurseForge 文件」后，重新安装或重新导入时${APP_NAME}会先尝试直接下载。`;
  const ON_FAILED = `已打开「自动下载受限的 CurseForge 文件」。${APP_NAME}直接下载不到的才会留在清单里；清单要是打开这一项之前留下的，点「重新安装」会再试一次。`;
  const ON_IMPORT = `已打开「自动下载受限的 CurseForge 文件」。清单里的是${APP_NAME}直接下载也下不到的文件，仍要手动下载。`;

  const flush = () =>
    act(async () => {
      await settle();
    });

  beforeEach(async () => {
    // 前面的用例在真实计时器上可能还排着一帧，换假时钟前先排空，免得这里的帧循环接不上
    await drainFrames();
    vi.useFakeTimers();
    resetRecorder(ipc);
    // 两个文件的编号各用例共用：上一个用例认领进暂存位的不能带到下一个
    resetModpackMock();
    stubResizeObserver();
    bootOverrides(ipc);
  });

  async function mount(origin: BlockedOrigin = "import", onAllFound: () => void = () => undefined) {
    view = render(
      <LauncherProvider>
        <OverlayProvider>
          <PickFolderProvider value={() => Promise.resolve(MOCK_DOWNLOADS_DIR)}>
            <BlockedFilesModal open files={FILES} origin={origin} onClose={() => undefined} onAllFound={onAllFound} />
          </PickFolderProvider>
          <OverlayRoot />
        </OverlayProvider>
      </LauncherProvider>,
    );
    await flush();
  }

  const restricted = (enabled: boolean) => bootOverrides(ipc, { config: configDto({ download_restricted_curseforge_files: enabled }) });

  function dialog(): HTMLElement {
    const dialogs = document.querySelectorAll<HTMLElement>('[role="dialog"]');
    if (dialogs.length !== 1) throw new Error(`应当恰好开着 1 个弹窗，实际 ${dialogs.length} 个`);
    return dialogs[0];
  }

  /** 列表上方的几段说明，按先后。 */
  function notes(): (string | null)[] {
    return [...dialog().querySelectorAll(`.${styles.blockedIntro}`)].map((p) => p.textContent);
  }

  /** 按钮此刻显示的文案：主按钮把几条文案叠在同一格里，只取当前那条。 */
  function shownLabel(button: HTMLButtonElement): string | null {
    const current = button.querySelector("[data-current]");
    return (current === null ? button : current).textContent;
  }

  const footerButtons = () => [...dialog().querySelectorAll<HTMLButtonElement>(`.${modalStyles.footer} button`)];

  /** 底栏各按钮此刻显示的文案，按先后。 */
  const footer = () => footerButtons().map(shownLabel);

  function footerButton(label: string): HTMLButtonElement {
    const found = footerButtons().filter((b) => shownLabel(b) === label);
    if (found.length !== 1) throw new Error(`底栏里「${label}」应当恰好 1 个，实际 ${found.length} 个`);
    return found[0];
  }

  it.each<[string, BlockedOrigin, boolean, string]>([
    ["关着、从导入打开：说在哪里打开、打开后重新安装或导入会先试直接下载", "import", false, OFF],
    ["关着、从失败任务打开：同一句", "failed", false, OFF],
    ["开着、从失败任务打开：说直接下载不到的才留在清单里，旧清单点「重新安装」会再试", "failed", true, ON_FAILED],
    ["开着、从导入打开：说清单里的直接下载也下不到，仍要手动下载", "import", true, ON_IMPORT],
  ])("开关%s", async (_name, origin, enabled, hint) => {
    restricted(enabled);
    await mount(origin);
    expect(notes()).toEqual([INTRO, hint]);
  });

  it("设置还没读到时不出这条提示，清单照常列出；读到之后按读到的取值补上", async () => {
    const config = deferred<ConfigDto>();
    ipc.overrides.set("get_config", () => config.promise);
    await mount();
    expect(notes()).toEqual([INTRO]);
    expect(document.querySelector('[role="dialog"]')?.textContent).toContain("locked.jar");

    config.resolve(configDto({ download_restricted_curseforge_files: true }));
    await flush();
    expect(notes()).toEqual([INTRO, ON_IMPORT]);
  });

  describe("底栏的「重新安装」", () => {
    it("开着、从失败任务打开：没找齐也有一颗次要的「重新安装」，排在「关闭」前面；点了交给调用方重排一次，不去认领文件", async () => {
      restricted(true);
      const onAllFound = vi.fn();
      await mount("failed", onAllFound);
      expect(footer()).toEqual(["重新安装", "关闭", "我已下载，重新检查"]);
      expect(footerButton("重新安装").dataset.variant).toBe("secondary");

      act(() => footerButton("重新安装").click());
      await flush();
      expect(onAllFound).toHaveBeenCalledTimes(1);
      expect(callsOf(ipc, "claim_blocked_files")).toEqual([]);
    });

    it.each<[string, BlockedOrigin, boolean]>([
      ["开着、从导入打开", "import", true],
      ["关着、从失败任务打开", "failed", false],
      ["关着、从导入打开", "import", false],
    ])("%s：没找齐时底栏只有「关闭」与检查", async (_name, origin, enabled) => {
      restricted(enabled);
      await mount(origin);
      expect(footer()).toEqual(["关闭", "我已下载，重新检查"]);
    });

    it("开着、从失败任务打开：找到一部分时次要的那颗还在；全部找到后主按钮换成「重新安装」，次要的拿掉，只剩一颗，点了重排一次", async () => {
      restricted(true);
      const onAllFound = vi.fn();
      seedMockDownloads(["locked.jar"]);
      await mount("failed", onAllFound);
      act(() => footerButton("我已下载，重新检查").click());
      await flush();
      expect(dialog().textContent).toContain("找到 1 个，还差 1 个。");
      expect(footer()).toEqual(["重新安装", "关闭", "我已下载，重新检查"]);

      seedMockDownloads(["dep.jar"]);
      act(() => footerButton("我已下载，重新检查").click());
      await flush();
      expect(dialog().textContent).toContain("2 个文件都已找到。");
      expect(footer()).toEqual(["关闭", "重新安装"]);
      expect(footerButton("重新安装").dataset.variant).toBe("primary");
      expect(onAllFound).not.toHaveBeenCalled();

      act(() => footerButton("重新安装").click());
      await flush();
      expect(onAllFound).toHaveBeenCalledTimes(1);
    });

    it("检查进行中次要的「重新安装」不可用但留着焦点；检查完全部找到时它拿掉，聚焦着它的焦点交给主按钮", async () => {
      restricted(true);
      const onAllFound = vi.fn();
      seedMockDownloads(["locked.jar", "dep.jar"]);
      const gate = deferred<void>();
      ipc.overrides.set("claim_blocked_files", (args) => gate.promise.then(() => mockInvoke("claim_blocked_files", args)));
      await mount("failed", onAllFound);
      act(() => footerButton("我已下载，重新检查").click());
      await flush();
      expect(footer()).toEqual(["重新安装", "关闭", "正在检查…"]);
      const early = footerButton("重新安装");
      expect(early.getAttribute("aria-disabled")).toBe("true");
      act(() => early.focus());
      act(() => early.click());
      await flush();
      expect(onAllFound).not.toHaveBeenCalled();
      expect(document.activeElement).toBe(early);

      await act(async () => {
        gate.resolve();
        await settle();
      });
      expect(early.isConnected).toBe(false);
      expect(footer()).toEqual(["关闭", "重新安装"]);
      expect(document.activeElement).toBe(footerButton("重新安装"));
      expect(onAllFound).not.toHaveBeenCalled();
    });
  });
});
