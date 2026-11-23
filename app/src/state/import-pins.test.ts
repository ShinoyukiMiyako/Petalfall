// @vitest-environment jsdom
// 导入中的整合包与侧栏固定：与新建实例一样，入队时侧栏有空位就以占位项排在侧栏末尾，只在派生状态里固定，装好才写进
// 固定列表；失败或取消时随占位项一起撤下，装好后原位转正。导入失败或取消、后端删掉半成品之后固定列表跟着重读；
// 后端删掉半成品时一并去掉了配置里对这个名字的当前选中与固定服务器记住的实例，这两样同样按盘上重读。
// 假后端的固定列表照后端的规矩：读的时候只给出还在实例库里的，写的时候每一项都要在。
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";
import { TOAST_COPY } from "../copy/flows";
import { CANCELLED_ERROR_CODE, IpcError } from "../ipc/errors";
import type { InstanceDto } from "../ipc/instances";
import type { IpcRecorder } from "../test/fake-bridge";
import { bootOverrides, callsOf, cancelledError, configDto, deferred, instanceDto, resetRecorder, settle, startedStore, type Deferred } from "../test/launcher";
import { resources } from "./resource";
import { pinnedInstances } from "./selectors";
import type { InstallModpackSpec, LauncherStore } from "./store";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../ipc/bridge", async (orig) => (await import("../test/fake-bridge")).fakeBridge(await orig(), ipc));

const PACK: InstallModpackSpec = {
  source: { kind: "file", path: "C:/packs/周末联机.mrpack" },
  name: "周末联机",
  cover: "grass",
  packName: "周末联机",
  packVersion: "1.0.0",
  modCount: 12,
  mcVersion: "1.21.1",
  loader: "fabric",
  loaderVersion: "0.16.10",
  excludedOptional: [],
};

let instances: InstanceDto[];
/** 配置里存着的固定列表。 */
let configPins: string[];
let store: LauncherStore;
let run: Deferred<unknown>;
/** 非 null 时实例列表的下一次请求挂在它上面：落地重扫停在半路。 */
let scan: Deferred<InstanceDto[]> | null;

const onDisk = (id: string) => instances.some((i) => i.id === id);

function pinsArg(args: Record<string, unknown>): string[] {
  const pins = args.pins;
  if (!Array.isArray(pins) || pins.some((p) => typeof p !== "string")) throw new Error("pins 应为字符串数组");
  return pins as string[];
}

/** 开机：配置里的固定列表由各用例给出；导入命令挂在 run 上。 */
async function boot(pins: string[]): Promise<void> {
  configPins = pins;
  store = await startedStore();
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(1_720_000_000_000);
  resetRecorder(ipc);
  instances = [instanceDto("生存"), instanceDto("创造")];
  scan = null;
  bootOverrides(ipc);
  ipc.overrides.set("list_instances", () => (scan === null ? instances : scan.promise));
  ipc.overrides.set("get_sidebar_pins", () => configPins.filter(onDisk));
  ipc.overrides.set("set_sidebar_pins", (args) => {
    const pins = pinsArg(args);
    const missing = pins.find((p) => !onDisk(p));
    if (missing !== undefined) throw new IpcError("invalid_argument", "侧栏固定列表不合法：要固定的实例不存在", { field: "pins", reason: "not_found", id: missing });
    configPins = pins;
    return pins;
  });
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
  store.stop();
  vi.useRealTimers();
});

const writes = () => callsOf(ipc, "set_sidebar_pins").map((args) => args.pins);
const entry = (id: string) => store.getState().instances.find((i) => i.id === id);
/** 侧栏此刻画出来的固定项：固定列表里还在实例列表中的那些。 */
const drawn = () => pinnedInstances(store.getState()).map((i) => i.id);

/** 从现在起每次状态变化时侧栏画出来的固定项，与实例列表里有没有这个整合包。 */
function watch(): { pins: string; listed: boolean }[] {
  const seen: { pins: string; listed: boolean }[] = [];
  onTestFinished(store.subscribe(() => seen.push({ pins: drawn().join("|"), listed: entry(PACK.name) !== undefined })));
  return seen;
}

/** 装到一半时重扫扫到了盘上的半成品目录（还没写版本 JSON，扫出来损坏），之后又读了一次固定列表。 */
async function scannedHalfway(): Promise<void> {
  instances = [...instances, instanceDto(PACK.name, { status: "broken", mc_version: null, loaders: [], activity: "installing" })];
  await store.actions.refreshInstances();
  await store.actions.reload("pins");
  await settle();
  expect(entry(PACK.name)).toMatchObject({ placeholder: false, status: "installing" });
}

/** 导入装好：盘上换成完整的实例，导入命令返回。 */
async function imported(): Promise<void> {
  instances = [...instances.filter((i) => i.id !== PACK.name), instanceDto(PACK.name, { loaders: [{ kind: "fabric", version: "0.16.10" }] })];
  run.resolve({ version_id: PACK.name });
  await settle();
  expect(store.getState().tasks.map((t) => t.status)).toEqual(["done"]);
  expect(entry(PACK.name)).toMatchObject({ placeholder: false, status: "ready" });
}

describe("导入失败或取消、半成品被删掉之后", () => {
  it.each([
    { ending: "失败", error: new IpcError("download_failed", "下载源暂时不可用") },
    { ending: "取消", error: new IpcError(CANCELLED_ERROR_CODE, "已取消") },
  ])("导入$ending：固定列表跟着重读，不再留着删掉的名字；下一次固定只写还在的实例，照常写成", async ({ error }) => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    // 同名实例早先在落樱之外删掉了，配置里的固定还留着：开机读到的固定列表滤掉了它
    await boot(["生存", PACK.name]);
    expect(store.getState().sidebarPins).toEqual(["生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    // 半成品目录建好之后后端认了这个名字，那条旧固定跟着回到固定列表里
    await scannedHalfway();
    expect(store.getState().sidebarPins).toEqual(["生存", PACK.name]);

    // 后端删掉半成品目录，再报导入的结局
    instances = instances.filter((i) => i.id !== PACK.name);
    run.reject(error);
    await settle();
    expect(store.getState().instances.map((i) => i.id)).toEqual(["生存", "创造"]);
    expect(store.getState().sidebarPins).toEqual(["生存"]);

    await store.actions.togglePin("创造");
    expect(writes()).toEqual([["生存", "创造"]]);
    expect(store.getState().sidebarPins).toEqual(["生存", "创造"]);
  });

  it.each([
    { ending: "失败", error: new IpcError("download_failed", "下载源暂时不可用") },
    { ending: "取消", error: new IpcError(CANCELLED_ERROR_CODE, "已取消") },
  ])("导入$ending：当前选中按盘上重读、固定服务器重取；再导入同名实例后选中它照常写进配置", async ({ error }) => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    // 配置里的当前选中还指着早先在落樱之外删掉的同名实例
    let selected: string | null = PACK.name;
    ipc.overrides.set("get_config", () => configDto({ selected_version: selected }));
    ipc.overrides.set("update_config", (args) => {
      if ("selectedVersion" in args) selected = args.selectedVersion as string | null;
      return null;
    });
    const fetched: string[] = [];
    onTestFinished(resources.subscribe("servers:pinned", async () => fetched.push("servers:pinned"), () => undefined));
    await boot([]);
    expect(store.getState().settings?.selectedVersion).toBe(PACK.name);
    await store.actions.installModpack(PACK);
    await settle();
    fetched.length = 0;

    // 后端删掉半成品、去掉配置里对这个名字的引用，再报导入的结局
    selected = null;
    run.reject(error);
    await settle();
    expect(store.getState().settings?.selectedVersion).toBeNull();
    expect(fetched).toEqual(["servers:pinned"]);

    // 同一会话里再导入同名实例，导入完成后选中它：前端不能还当它已是当前实例而跳过写入
    run = deferred<unknown>();
    await store.actions.installModpack(PACK);
    await settle();
    instances = [...instances, instanceDto(PACK.name, { loaders: [{ kind: "fabric", version: "0.16.10" }] })];
    run.resolve({ version_id: PACK.name });
    await settle();
    await store.actions.selectInstance(PACK.name);
    expect(callsOf(ipc, "update_config")).toEqual([{ selectedVersion: PACK.name }]);
    expect(selected).toBe(PACK.name);
  });
});

describe("导入进行中：与新建一样，入队就进侧栏", () => {
  it("入队即以占位项排在侧栏末尾，只在派生状态里固定、不写固定列表；半成品被扫到后照旧排在末尾", async () => {
    await boot(["生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    expect(entry(PACK.name)).toMatchObject({ placeholder: true, status: "installing", pinned: true });
    expect(store.getState().sidebarPins).toEqual(["生存", PACK.name]);
    expect(store.getState().unsavedPins).toEqual([PACK.name]);
    expect(drawn()).toEqual(["生存", PACK.name]);

    // 半成品目录建好之后后端认这个名字，但导入失败或取消时它又会被删掉：照旧不写进固定列表
    await scannedHalfway();
    expect(entry(PACK.name)).toMatchObject({ pinned: true });
    expect(store.getState().unsavedPins).toEqual([PACK.name]);
    expect(drawn()).toEqual(["生存", PACK.name]);
    expect(writes()).toEqual([]);
  });

  it("导入完成：原位转正，固定列表只写一次；侧栏从头到尾就是这两项，不缺、不重复", async () => {
    await boot(["生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    const seen = watch();
    await imported();
    expect(entry(PACK.name)).toMatchObject({ pinned: true });
    expect(store.getState().instances.filter((i) => i.id === PACK.name)).toHaveLength(1);
    expect(writes()).toEqual([["生存", PACK.name]]);
    expect(store.getState().sidebarPins).toEqual(["生存", PACK.name]);
    expect(store.getState().unsavedPins).toEqual([]);
    expect(seen.length).toBeGreaterThan(0);
    expect(new Set(seen.map((s) => s.pins))).toEqual(new Set([`生存|${PACK.name}`]));
    expect(store.getState().toasts.map((t) => [t.tone, t.title])).toEqual([["success", `「${PACK.name}」已导入`]]);
  });

  it("导入失败：占位项留到落地重扫回来，随后与侧栏里的这一项在同一次更新里撤下，固定列表一次不写", async () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    await boot(["生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    const seen = watch();
    scan = deferred<InstanceDto[]>();
    run.reject(new IpcError("download_failed", "下载源暂时不可用"));
    await settle();
    expect(store.getState().tasks.map((t) => t.status)).toEqual(["failed"]);
    expect(drawn()).toEqual(["生存", PACK.name]);

    scan.resolve(instances);
    scan = null;
    await settle();
    expect(entry(PACK.name)).toBeUndefined();
    expect(store.getState().sidebarPins).toEqual(["生存"]);
    expect(store.getState().unsavedPins).toEqual([]);
    expect(writes()).toEqual([]);
    // 侧栏画着它，实例列表里就有它：不会先从侧栏消失、过一拍才从实例库消失，反过来也不会
    expect(seen.every((s) => s.pins.split("|").includes(PACK.name) === s.listed)).toBe(true);
    expect(seen.map((s) => s.pins).at(-1)).toBe("生存");
  });

  it("运行中取消：命令以取消返回后与失败一样撤下，固定列表一次不写", async () => {
    await boot(["生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    const seen = watch();
    const [task] = store.getState().tasks;
    await store.actions.cancelTask(task.id);
    await settle();
    expect(callsOf(ipc, "cancel_task")).toHaveLength(1);
    expect(store.getState().tasks.map((t) => t.status)).toEqual(["cancelled"]);
    expect(entry(PACK.name)).toBeUndefined();
    expect(store.getState().sidebarPins).toEqual(["生存"]);
    expect(store.getState().unsavedPins).toEqual([]);
    expect(writes()).toEqual([]);
    expect(seen.every((s) => s.pins.split("|").includes(PACK.name) === s.listed)).toBe(true);
  });

  it("排队中取消：当场撤下，不等重扫", async () => {
    instances = [instanceDto("生存"), instanceDto("创造"), instanceDto("空岛")];
    ipc.overrides.set("repair_instance", () => new Promise(() => undefined));
    await boot(["生存"]);
    // 三个补全占满并行通道，导入排在后面
    for (const id of ["生存", "创造", "空岛"]) await store.actions.repairInstance(id);
    await store.actions.installModpack(PACK);
    await settle();
    const pack = store.getState().tasks.find((t) => t.kind === "modpack");
    if (pack === undefined) throw new Error("导入没有入队");
    expect(pack.status).toBe("queued");
    expect(drawn()).toEqual(["生存", PACK.name]);

    const listed = callsOf(ipc, "list_instances").length;
    await store.actions.cancelTask(pack.id);
    expect(entry(PACK.name)).toBeUndefined();
    expect(drawn()).toEqual(["生存"]);
    expect(callsOf(ipc, "list_instances")).toHaveLength(listed);
    expect(callsOf(ipc, "install_modpack")).toEqual([]);
    expect(writes()).toEqual([]);
  });

  it("导入中取消固定、再固定：只改装好后的打算，不写固定列表；装好时按最后的打算落地", async () => {
    await boot(["生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    await store.actions.togglePin(PACK.name);
    expect(entry(PACK.name)).toMatchObject({ placeholder: true, pinned: false });
    expect(store.getState().sidebarPins).toEqual(["生存"]);

    await scannedHalfway();
    expect(store.getState().sidebarPins).toEqual(["生存"]);
    await store.actions.togglePin(PACK.name);
    expect(store.getState().sidebarPins).toEqual(["生存", PACK.name]);
    await store.actions.togglePin(PACK.name);
    expect(store.getState().sidebarPins).toEqual(["生存"]);
    expect(writes()).toEqual([]);

    await imported();
    expect(writes()).toEqual([]);
    expect(entry(PACK.name)).toMatchObject({ pinned: false });
    expect(store.getState().sidebarPins).toEqual(["生存"]);
  });

  it("导入中拖动排序：只写已落库的项，导入中的那一项照旧排在末尾；装好时追加到末尾", async () => {
    await boot(["生存", "创造"]);
    await store.actions.installModpack(PACK);
    await settle();
    await store.actions.setSidebarPins(["创造", PACK.name, "生存"]);
    expect(writes()).toEqual([["创造", "生存"]]);
    expect(store.getState().sidebarPins).toEqual(["创造", "生存", PACK.name]);

    await imported();
    expect(writes()).toEqual([["创造", "生存"], ["创造", "生存", PACK.name]]);
    expect(store.getState().sidebarPins).toEqual(["创造", "生存", PACK.name]);
  });

  it("入队时侧栏已满 5 个：不进侧栏，导入中点固定只给上限提示；之后腾出空位、装好也不自动固定", async () => {
    const full = ["生存", "创造", "空岛", "末地", "下界"];
    instances = full.map((id) => instanceDto(id));
    await boot(full);
    await store.actions.installModpack(PACK);
    await settle();
    expect(entry(PACK.name)).toMatchObject({ placeholder: true, pinned: false });
    expect(store.getState().sidebarPins).toEqual(full);
    expect(store.getState().unsavedPins).toEqual([]);

    await store.actions.togglePin(PACK.name);
    expect(store.getState().toasts.map((t) => [t.tone, t.title])).toEqual([["warning", TOAST_COPY.pinLimit]]);
    expect(store.getState().sidebarPins).toEqual(full);

    // 与新建一样按入队那一刻算：之后腾出空位也不自己补进来，要固定就手动点
    await store.actions.togglePin("下界");
    expect(store.getState().sidebarPins).toEqual(full.slice(0, 4));

    await imported();
    expect(writes()).toEqual([full.slice(0, 4)]);
    expect(entry(PACK.name)).toMatchObject({ pinned: false });
    expect(store.getState().sidebarPins).toEqual(full.slice(0, 4));
  });

  it("本来就已固定的（配置里留着同名旧固定、半成品被扫到后读了回来）：取消固定从固定列表里去掉，原位撤下，装好后不再固定", async () => {
    await boot([PACK.name, "生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    // 开机读到的列表滤掉了盘上没有的旧固定，导入中的这一项先按自动固定排在末尾
    expect(store.getState().sidebarPins).toEqual(["生存", PACK.name]);
    await scannedHalfway();
    expect(store.getState().sidebarPins).toEqual([PACK.name, "生存"]);
    expect(store.getState().unsavedPins).toEqual([]);

    const seen = watch();
    await store.actions.togglePin(PACK.name);
    expect(writes()).toEqual([["生存"]]);
    expect(store.getState().sidebarPins).toEqual(["生存"]);
    expect(entry(PACK.name)).toMatchObject({ pinned: false });
    // 写入回来之前留在原位，不先跳到末尾
    expect([...new Set(seen.map((s) => s.pins))]).toEqual([`${PACK.name}|生存`, "生存"]);

    await imported();
    expect(writes()).toEqual([["生存"]]);
    expect(store.getState().sidebarPins).toEqual(["生存"]);
    expect(entry(PACK.name)).toMatchObject({ pinned: false });
  });

  it("配置里留着同名旧固定、半成品被扫到后读了回来，导入中没动过它：导入结束后端去掉那一项，落地重读后按自动固定追加到末尾", async () => {
    await boot([PACK.name, "生存"]);
    await store.actions.installModpack(PACK);
    await settle();
    await scannedHalfway();
    expect(store.getState().sidebarPins).toEqual([PACK.name, "生存"]);

    // 导入成功返回之前，后端去掉了配置里与新实例同名的旧固定
    configPins = configPins.filter((p) => p !== PACK.name);
    await imported();
    expect(writes()).toEqual([["生存", PACK.name]]);
    expect(store.getState().sidebarPins).toEqual(["生存", PACK.name]);
    expect(store.getState().unsavedPins).toEqual([]);
    expect(entry(PACK.name)).toMatchObject({ pinned: true });
  });
});
