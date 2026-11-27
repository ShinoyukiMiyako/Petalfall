// @vitest-environment jsdom
// @module-tag heavy-page
// 导入中的整合包在侧栏里与新建的实例一样：入队时侧栏有空位就排在固定区末尾，失败或取消后撤下，装好后原位转正。
// 导入期间菜单里的取消固定照常可用：还没写进固定列表的只改装好后的打算；本来就已固定（配置里留着同名的旧固定，
// 半成品目录被扫到后后端就认了它）的从固定列表里去掉。上移下移只重排已落库的项。
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubCanvas } from "../art/illustrations/test-canvas";
import { SIDEBAR_COPY } from "../copy/shell";
import { IpcError } from "../ipc/errors";
import type { InstanceDto } from "../ipc/instances";
import { LauncherProvider, useLauncher, type LauncherApi } from "../state/launcher";
import { keydown, render, SETTLE_FRAMES, stepFrames, stubMatchMedia, stubResizeObserver } from "../test/dom";
import type { IpcRecorder } from "../test/fake-bridge";
import { bootOverrides, callsOf, cancelledError, deferred, instanceDto, resetRecorder, settle, type Deferred } from "../test/launcher";
import { ChromeProvider, type ChromeApi } from "./chrome";
import { OverlayRoot } from "./overlay";
import { ShellProvider } from "./shell";
import { Sidebar } from "./Sidebar";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../ipc/bridge", async (orig) => (await import("../test/fake-bridge")).fakeBridge(await orig(), ipc));

const PACK = "周末联机";
const NOW = Date.UTC(2026, 9, 31, 12);
const chrome: ChromeApi = { scrollContentToTop: vi.fn(), focusContent: vi.fn() };

let launcher: LauncherApi;
let instances: InstanceDto[];
/** 配置里存着的固定列表：读的时候像后端一样只给出盘上还在的。 */
let configPins: string[];
let run: Deferred<unknown>;

function Probe() {
  launcher = useLauncher();
  return null;
}

const flush = () =>
  act(async () => {
    await settle(20);
  });

async function mount(): Promise<void> {
  render(
    <LauncherProvider>
      <ShellProvider>
        <ChromeProvider value={chrome}>
          <Probe />
          <Sidebar />
          <OverlayRoot />
        </ChromeProvider>
      </ShellProvider>
    </LauncherProvider>,
  );
  await flush();
}

async function importPack(): Promise<void> {
  await act(async () => {
    await launcher.actions.installModpack({
      source: { kind: "file", path: `D:/整合包/${PACK}.mrpack` },
      name: PACK,
      cover: "grass",
      packName: PACK,
      packVersion: "1.0.0",
      modCount: 12,
      mcVersion: "1.21.1",
      loader: "fabric",
      loaderVersion: "0.16.10",
      excludedOptional: [],
    });
    await settle();
  });
  expect(launcher.state.tasks.map((t) => [t.kind, t.status])).toEqual([["modpack", "running"]]);
}

/**
 * 导入一直不返回；装到一半时重扫扫到半成品目录，之后又读了一次固定列表，配置里那条同名旧固定跟着回来，
 * 导入中的实例就是已固定的。
 */
async function importPinned(): Promise<void> {
  configPins = ["生存", PACK, "创造"];
  await mount();
  await importPack();
  instances = [
    ...instances,
    instanceDto(PACK, {
      status: "broken",
      mc_version: null,
      loaders: [],
      mod_count: null,
      world_count: null,
      activity: "installing",
      incomplete: { kind: "modpack", outcome: "pending", started_at: NOW, spec: null, error: null },
    }),
  ];
  await act(async () => {
    await launcher.actions.refreshInstances();
    await launcher.actions.reload("pins");
    await settle();
  });
  expect(launcher.state.sidebarPins).toEqual(["生存", PACK, "创造"]);
  expect(launcher.state.unsavedPins).toEqual([]);
}

function pin(id: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-sidebar-item="pin:${id}"]`);
  if (el === null) throw new Error(`侧栏没有固定项 ${id}`);
  return el;
}

function rowOf(id: string): HTMLLIElement {
  const row = pin(id).closest("li");
  if (row === null) throw new Error(`固定项 ${id} 不在列表项里`);
  return row;
}

const pinOrder = () => [...document.querySelectorAll<HTMLElement>("[data-sidebar-item^='pin:']")].map((el) => el.dataset.sidebarItem);

/** 键盘打开 id 那一项的菜单，返回按名称取其中条目的函数。 */
async function openMenu(id: string): Promise<(label: string) => HTMLElement> {
  act(() => pin(id).focus());
  keydown(pin(id), "ContextMenu");
  await flush();
  const menu = [...document.querySelectorAll("[role='menu']")].find((el) => el.getAttribute("aria-label") === SIDEBAR_COPY.menuLabel(id) && el.closest("[data-exiting]") === null);
  if (menu === undefined) throw new Error(`没有 ${id} 的菜单`);
  return (label) => {
    const found = [...menu.querySelectorAll<HTMLElement>("[role='menuitem']")].find((el) => el.textContent === label);
    if (found === undefined) throw new Error(`${id} 的菜单里没有 ${label}`);
    return found;
  };
}

beforeEach(() => {
  vi.useFakeTimers({ now: NOW });
  resetRecorder(ipc);
  stubResizeObserver();
  stubMatchMedia(false);
  stubCanvas();
  instances = [instanceDto("生存"), instanceDto("创造")];
  configPins = ["生存", "创造"];
  bootOverrides(ipc, { instances });
  ipc.overrides.set("list_instances", () => instances);
  ipc.overrides.set("get_sidebar_pins", () => configPins.filter((id) => instances.some((i) => i.id === id)));
  // 写下的整组固定像后端一样存进配置：新建、导入落地时前端会按后端重读一次
  ipc.overrides.set("set_sidebar_pins", (args) => (configPins = args.pins as string[]));
  ipc.overrides.set("art_status", () => ({ source: null, panorama: false }));
  ipc.overrides.set("updater_info", () => ({ current_version: "0.1.0", supported: true, unsupported_reason: null, available: null, downloaded_version: null, install_on_exit: false }));
  // 导入装好时要写封面：实例设置按传入的原样存下
  const settings = { description: null, cover: null, cover_image: null, background: "follow", background_image: null, isolation: "follow_global", memory_mb: null, java: "auto", jvm_args: "", resolution: { width: 854, height: 480 }, fullscreen: false };
  ipc.overrides.set("get_instance_settings", () => settings);
  ipc.overrides.set("set_instance_settings", (args) => args.settings);
  run = deferred<unknown>();
  ipc.overrides.set("install_modpack", () => run.promise);
  ipc.overrides.set("cancel_task", () => {
    run.reject(cancelledError());
    return true;
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("侧栏：导入中的整合包与新建的实例一样入队就进来", () => {
  it("入队即排在固定区末尾，像新固定的项一样从小处弹入；不写固定列表", async () => {
    await mount();
    expect(pinOrder()).toEqual(["pin:生存", "pin:创造"]);
    await importPack();
    expect(pinOrder()).toEqual(["pin:生存", "pin:创造", `pin:${PACK}`]);
    // jsdom 里动画只在推进帧时走：新进来的项停在入场起点，随固定区一起在的项直接是终点
    expect(rowOf(PACK).style.opacity).toBe("0");
    expect(rowOf("生存").style.opacity).toBe("1");
    await stepFrames(SETTLE_FRAMES);
    expect(rowOf(PACK).style.opacity).toBe("1");
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([]);
  });

  it.each([
    { ending: "失败", status: "failed", end: () => run.reject(new IpcError("download_failed", "下载源暂时不可用")) },
    { ending: "取消", status: "cancelled", end: () => launcher.actions.cancelTask(launcher.state.tasks[0].id) },
  ])("导入$ending：这一项从侧栏撤下，其余项不动，固定列表一次不写", async ({ status, end }) => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    await mount();
    await importPack();
    await stepFrames(SETTLE_FRAMES);
    const row = rowOf(PACK);
    await act(async () => {
      await end();
      await settle(20);
    });
    expect(launcher.state.tasks.map((t) => t.status)).toEqual([status]);
    await stepFrames(SETTLE_FRAMES);
    expect(row.isConnected).toBe(false);
    expect(pinOrder()).toEqual(["pin:生存", "pin:创造"]);
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([]);
  });

  it("导入完成：原位转正，留着的还是同一个节点，不重播入场也不退场重来；固定列表写一次", async () => {
    await mount();
    await importPack();
    await stepFrames(SETTLE_FRAMES);
    const row = rowOf(PACK);
    instances = [...instances, instanceDto(PACK, { loaders: [{ kind: "fabric", version: "0.16.10" }] })];
    await act(async () => {
      run.resolve({ version_id: PACK });
      await settle(20);
    });
    expect(launcher.state.tasks.map((t) => t.status)).toEqual(["done"]);
    expect(launcher.state.instances.find((i) => i.id === PACK)).toMatchObject({ placeholder: false, status: "ready", pinned: true });
    expect(rowOf(PACK)).toBe(row);
    expect(row.style.opacity).toBe("1");
    await stepFrames(SETTLE_FRAMES);
    expect(row.isConnected).toBe(true);
    expect(row.style.opacity).toBe("1");
    expect(document.querySelectorAll(`[data-sidebar-item="pin:${PACK}"]`)).toHaveLength(1);
    expect(pinOrder()).toEqual(["pin:生存", "pin:创造", `pin:${PACK}`]);
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([{ pins: ["生存", "创造", PACK] }]);
  });

  it("导入中从菜单取消固定：照常可点，只改装好后的打算，不写固定列表也不弹通知；还没落库，上移下移置灰", async () => {
    await mount();
    await importPack();
    const entry = await openMenu(PACK);
    expect(entry(SIDEBAR_COPY.moveUp).getAttribute("aria-disabled")).toBe("true");
    expect(entry(SIDEBAR_COPY.moveDown).getAttribute("aria-disabled")).toBe("true");
    const unpin = entry(SIDEBAR_COPY.unpin);
    expect(unpin.getAttribute("aria-disabled")).toBeNull();
    act(() => unpin.click());
    await flush();
    expect(launcher.state.sidebarPins).toEqual(["生存", "创造"]);
    expect(launcher.state.instances.find((i) => i.id === PACK)).toMatchObject({ placeholder: true, pinned: false });
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([]);
    expect(launcher.state.toasts).toEqual([]);
  });
});

describe("侧栏固定项菜单：导入中的实例本来就已固定", () => {
  it("取消固定照常可点，从固定列表里去掉，装好后也不再固定；别的固定项照常能取消", async () => {
    await importPinned();

    const unpin = (await openMenu(PACK))(SIDEBAR_COPY.unpin);
    expect(unpin.getAttribute("aria-disabled")).toBeNull();
    act(() => unpin.click());
    await flush();
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([{ pins: ["生存", "创造"] }]);
    expect(launcher.state.sidebarPins).toEqual(["生存", "创造"]);
    expect(launcher.state.toasts).toEqual([]);

    const other = (await openMenu("创造"))(SIDEBAR_COPY.unpin);
    expect(other.getAttribute("aria-disabled")).toBeNull();
    act(() => other.click());
    await flush();
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([{ pins: ["生存", "创造"] }, { pins: ["生存"] }]);

    instances = instances.map((i) => (i.id === PACK ? instanceDto(PACK, { loaders: [{ kind: "fabric", version: "0.16.10" }] }) : i));
    await act(async () => {
      run.resolve({ version_id: PACK });
      await settle(20);
    });
    expect(launcher.state.tasks.map((t) => t.status)).toEqual(["done"]);
    expect(callsOf(ipc, "set_sidebar_pins")).toHaveLength(2);
    expect(launcher.state.sidebarPins).toEqual(["生存"]);
  });

  it("上移下移只重排、状态层不拦：照常可用，写下新顺序", async () => {
    await importPinned();

    const entry = await openMenu(PACK);
    expect(entry(SIDEBAR_COPY.moveUp).getAttribute("aria-disabled")).toBeNull();
    const down = entry(SIDEBAR_COPY.moveDown);
    expect(down.getAttribute("aria-disabled")).toBeNull();
    act(() => down.click());
    await flush();
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([{ pins: ["生存", "创造", PACK] }]);
    expect(launcher.state.toasts).toEqual([]);
  });
});
