// @vitest-environment jsdom
// @module-tag heavy-page
// 设置页常规分区：界面语言的选择与写入，以及分区按片订阅（游戏运行时每批日志都换一次状态，设置行不该跟着重渲染）。
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubCanvas } from "../../../art/illustrations/test-canvas";
import type { ConfigDto } from "../../../ipc/config";
import { GAME_LOG_EVENT } from "../../../ipc/launch";
import { APP_NAME } from "../../../lib/app-info";
import { OverlayRoot } from "../../../shell/overlay";
import { ShellProvider, useNav, type NavApi } from "../../../shell/shell";
import { AppearanceProvider, DEFAULT_APPEARANCE } from "../../../state/appearance";
import { LauncherProvider, useLauncher, type LauncherApi } from "../../../state/launcher";
import { render, stubMatchMedia, stubResizeObserver, type Rendered } from "../../../test/dom";
import type { IpcArgs, IpcRecorder } from "../../../test/fake-bridge";
import { bootOverrides, callsOf, configDto, emit, instanceDto, resetRecorder, settle } from "../../../test/launcher";
import SettingsPage from "../index";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../../../ipc/bridge", async (orig) => (await import("../../../test/fake-bridge")).fakeBridge(await orig(), ipc));

// 此刻挂着让位的容器：jsdom 不做布局，让位本身在这里测不了，只核对挂在了哪一行；挂载照常交给真实现
const reflowing = vi.hoisted(() => new Set<Element>());
vi.mock("../../../lib/reflow", async (orig) => {
  const real = await orig<typeof import("../../../lib/reflow")>();
  const { useCallback } = await import("react");
  return {
    ...real,
    useReflow: <T extends HTMLElement>(spring?: Parameters<typeof real.useReflow>[0]) => {
      const ref = real.useReflow<T>(spring);
      return useCallback(
        (el: T | null) => {
          if (el === null) return;
          reflowing.add(el);
          const stop = ref(el);
          return () => {
            reflowing.delete(el);
            if (typeof stop === "function") stop();
          };
        },
        [ref],
      );
    },
  };
});

const renders = vi.hoisted(() => ({ SettingRow: 0 }));

// 设置行换成计数外壳：分区里的每个设置行组件只要重渲染，都会带着新的 props 重渲染它的 SettingRow。
vi.mock("../../../ui/SettingRow", async (orig) => {
  const mod = await orig<typeof import("../../../ui/SettingRow")>();
  return {
    ...mod,
    SettingRow: (props: Parameters<typeof mod.SettingRow>[0]) => {
      renders.SettingRow += 1;
      return mod.SettingRow(props);
    },
  };
});

const ZH_DESC = "界面、Toast 与崩溃诊断的文案语言";
const EN_DESC = "English UI is in preview; untranslated strings stay in Chinese.";

let view: Rendered | undefined;
let nav: NavApi;
let launcher: LauncherApi;
let config: ConfigDto;

// 整份订阅放在与页面平级的探针里：放进页面的父级会让整页随每次状态变化重渲染，数不出分区自己的订阅
function Probe() {
  launcher = useLauncher();
  return null;
}

function Harness() {
  nav = useNav();
  return nav.route.page === "settings" ? <SettingsPage route={nav.route} /> : null;
}

const flush = () =>
  act(async () => {
    await settle(20);
  });

async function mountGeneral() {
  view = render(
    <AppearanceProvider initial={DEFAULT_APPEARANCE}>
      <LauncherProvider>
        <ShellProvider>
          <Probe />
          <Harness />
          <OverlayRoot />
        </ShellProvider>
      </LauncherProvider>
    </AppearanceProvider>,
  );
  await flush();
  act(() => nav.go({ page: "settings", section: "general" }));
  await flush();
}

const all = <E extends Element = HTMLElement>(selector: string): E[] => [...document.querySelectorAll<E>(selector)];

function languageTrigger(): HTMLButtonElement {
  const found = all<HTMLButtonElement>('button[aria-haspopup="listbox"]').find((b) => b.getAttribute("aria-label") === "语言");
  if (found === undefined) throw new Error("找不到语言下拉");
  return found;
}

/** 语言所在的卡片：卡片标题「界面」。 */
function interfaceCard(): HTMLElement {
  const heading = all("h2, h3").find((h) => h.textContent === "界面");
  const card = heading?.closest<HTMLElement>('[class*="card"]');
  if (card === undefined || card === null) throw new Error("找不到「界面」卡片");
  return card;
}

/** 界面卡片里写着这句说明的元素所标的语言；没有任何祖先标语言时为 null。 */
function descLang(text: string): string | null {
  const el = [...interfaceCard().querySelectorAll<HTMLElement>("*")].find((e) => e.textContent === text && [...e.children].every((c) => c.textContent !== text));
  if (el === undefined) throw new Error(`界面卡片里没有这句说明：${text}`);
  const tagged = el.closest("[lang]");
  return tagged === null ? null : tagged.getAttribute("lang");
}

const click = async (el: HTMLElement) => {
  act(() => el.click());
  await flush();
};

/** 打开着的下拉里的选项（退场动画中的不算：假定时器下动画不会走完，上一次的列表会留在文档里）。 */
const openOptions = () => all('[role="option"]').filter((o) => o.closest("[data-exiting]") === null);

async function pickLanguage(label: string) {
  await click(languageTrigger());
  const option = openOptions().find((o) => o.textContent?.startsWith(label));
  if (option === undefined) throw new Error(`下拉里没有「${label}」`);
  await click(option);
}

beforeEach(() => {
  vi.useFakeTimers();
  resetRecorder(ipc);
  stubResizeObserver();
  stubMatchMedia(false);
  stubCanvas();
  renders.SettingRow = 0;
  config = configDto();
  bootOverrides(ipc, { instances: [instanceDto("生存"), instanceDto("创造")] });
  ipc.overrides.set("get_config", () => config);
  ipc.overrides.set("update_config", (args: IpcArgs) => {
    if (args.language !== undefined) config = { ...config, language: args.language as ConfigDto["language"] };
    if (args.afterLaunch !== undefined) config = { ...config, after_launch: args.afterLaunch as ConfigDto["after_launch"] };
    return null;
  });
  ipc.overrides.set("art_status", () => ({ source: null, panorama: false }));
  vi.spyOn(console, "error").mockImplementation(() => undefined);
});

afterEach(() => {
  view?.unmount();
  view = undefined;
  document.documentElement.removeAttribute("data-sk-mode");
  document.documentElement.removeAttribute("data-sk-accent");
  vi.useRealTimers();
});

describe("设置页常规分区：界面语言", () => {
  it("「界面」卡片里是语言下拉，默认简体中文；下拉只给有译文的简体中文，还没有译文的 English 不出现", async () => {
    await mountGeneral();
    const card = interfaceCard();
    expect(card.contains(languageTrigger())).toBe(true);
    expect(languageTrigger().textContent).toBe("简体中文");
    expect(card.textContent).toContain(ZH_DESC);
    expect(descLang(ZH_DESC)).toBe("zh-CN");
    await click(languageTrigger());
    const options = openOptions().map((o) => ({ text: o.textContent, selected: o.getAttribute("aria-selected") }));
    expect(options).toEqual([{ text: "简体中文zh-CN", selected: "true" }]);
  });

  it("配置里存着早先选的 English：下拉与说明按中文显示、不报错；点简体中文也不写回，存的 en-US 原样留着", async () => {
    config = configDto({ language: "en-US" });
    await mountGeneral();
    expect(launcher.state.settings?.language).toBe("en-US");
    expect(languageTrigger().textContent).toBe("简体中文");
    expect(interfaceCard().textContent).toContain(ZH_DESC);
    expect(interfaceCard().textContent).not.toContain(EN_DESC);
    expect(descLang(ZH_DESC)).toBe("zh-CN");

    await pickLanguage("简体中文");
    expect(openOptions()).toEqual([]);
    expect(callsOf(ipc, "update_config")).toEqual([]);
    expect(launcher.state.settings?.language).toBe("en-US");
    expect(config.language).toBe("en-US");
    expect(languageTrigger().textContent).toBe("简体中文");
  });
});

describe("设置页常规分区：游戏启动后关闭与自动备份", () => {
  const NOTE = `「游戏启动后」选了「关闭${APP_NAME}」，游戏退出时不会自动备份存档`;
  // 退场中的提醒行带 inert：假定时器下淡出走不完，节点会留在文档里，不算在页面上
  const page = () => {
    const copy = document.body.cloneNode(true) as HTMLElement;
    for (const leaving of copy.querySelectorAll("[inert]")) leaving.remove();
    return copy.textContent;
  };
  const noteHost = () => {
    const note = [...document.querySelectorAll<HTMLElement>("span")].find((el) => el.textContent === NOTE);
    if (note === undefined || note.parentElement === null) throw new Error("没有提醒行");
    return note.parentElement;
  };
  const radio = (label: string) => {
    const found = [...document.querySelectorAll<HTMLElement>('[role="radio"]')].find((el) => el.textContent === label);
    if (found === undefined) throw new Error(`找不到选项「${label}」`);
    return found;
  };
  const choose = async (label: string) => {
    act(() => radio(label).click());
    await flush();
  };

  it("选最小化：说明写进程起来后，不写游戏窗口出现后（拿到进程就最小化，这时窗口还没出来）", async () => {
    await mountGeneral();
    await choose("最小化");
    expect(page()).toContain(`游戏进程起来后${APP_NAME}缩到任务栏，结束后再弹回来`);
    expect(page()).not.toContain("游戏窗口出现后");
  });

  it("开着自动备份时选关闭：设置下方写明不会备份，换回别的选项这行就没了", async () => {
    await mountGeneral();
    expect(page()).not.toContain(NOTE);
    await choose(`关闭${APP_NAME}`);
    expect(page()).toContain(NOTE);
    await choose("最小化");
    expect(page()).not.toContain(NOTE);
  });

  it("提醒行出现时淡入并从略小处落到位，换走时退场期间不可交互", async () => {
    await mountGeneral();
    act(() => radio(`关闭${APP_NAME}`).click());
    expect([noteHost().style.opacity, noteHost().style.transform]).toEqual(["0", "scale(0.98)"]);
    await flush();
    const host = noteHost();
    await choose("最小化");
    expect(host.hasAttribute("inert")).toBe(true);
  });

  it("配置里本来就是关闭：打开设置页就有这行，跟着页面入场，不单独淡入", async () => {
    config = configDto({ after_launch: "close" });
    await mountGeneral();
    expect(page()).toContain(NOTE);
    expect(noteHost().style.opacity).toBe("1");
  });

  it("自动备份关着时选关闭：没有要提醒的", async () => {
    config = configDto({ after_launch: "close", world_backup: { auto: false, keep_per_world: 5, max_total_gb: 20 } });
    await mountGeneral();
    expect(page()).not.toContain(NOTE);
  });

  it("「游戏启动后」的说明随选项变高变矮：这一行开着让位，刚点的分段控件从旧位置滑到新位置，不随竖向居中一帧挪开；说明不变的下一行不开", async () => {
    await mountGeneral();
    /** 标题为 title 的设置行：行 > 标题说明列 > 标题。 */
    const row = (title: string) => {
      const heading = [...document.querySelectorAll<HTMLElement>('[id$="-title"]')].find((el) => el.textContent === title);
      const found = heading?.parentElement?.parentElement;
      if (found === undefined || found === null) throw new Error(`找不到设置行「${title}」`);
      return found;
    };
    const afterLaunch = row("游戏启动后");
    expect(afterLaunch.querySelector('[role="radiogroup"]')?.getAttribute("aria-label")).toBe("游戏启动后");
    expect(reflowing.has(afterLaunch)).toBe(true);
    expect(reflowing.has(row("游戏语言跟随系统语言"))).toBe(false);
    expect(reflowing.has(row("跳过首次无障碍引导"))).toBe(false);
    // 换到会多出提醒行的选项后让位仍挂在同一行上
    await choose(`关闭${APP_NAME}`);
    expect(page()).toContain(NOTE);
    expect(reflowing.has(row("游戏启动后"))).toBe(true);
  });
});

describe("设置页常规分区：按片订阅", () => {
  it("游戏运行中 20 批日志：五个设置行一次都不重渲染；改了语言之后才重渲染", async () => {
    ipc.overrides.set("launch_game", () => ({ pid: 4242 }));
    await mountGeneral();
    expect(languageTrigger().textContent).toBe("简体中文");
    await act(async () => {
      await launcher.actions.launch("创造");
      await settle();
    });
    expect(launcher.state.session).toMatchObject({ instanceId: "创造", phase: "running" });
    const before = renders.SettingRow;
    for (let i = 1; i <= 20; i++) {
      act(() => {
        emit(GAME_LOG_EVENT, { version_id: "创造", lines: [{ level: "info", source: "game", text: `第 ${i} 行` }] });
      });
    }
    expect(launcher.state.session?.logs).toHaveLength(20);
    expect(renders.SettingRow).toBe(before);

    // 下拉里只有简体中文，换不出别的语言：经状态层改，语言行订阅着这一项
    await act(async () => {
      await launcher.actions.updateSettings({ language: "en-US" });
      await settle();
    });
    expect(renders.SettingRow).toBeGreaterThan(before);
  });
});
