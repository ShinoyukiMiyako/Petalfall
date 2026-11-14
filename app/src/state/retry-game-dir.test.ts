// @vitest-environment jsdom
// 切换游戏目录之后的重试：任务只存实例名，同名实例在别的游戏目录里是另一个实例。任务记下入队那一刻的游戏目录，
// 目录对不上（或旧日志里不知道）时单条重试拒绝并说明、批量重试不收、断网恢复的计数不算；按实例名查当前目录扫描结果的
// 两个判断（失败新建的残留、中断前已装完）排在这道核对之后。目录对得上的照常重试，命令带上发起时的目录供后端兜底核对。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TASK_JOURNAL_DEBOUNCE_MS } from "../domain/limits";
import { IpcError } from "../ipc/errors";
import type { InstallResultDto, InstanceDto, RepairResultDto } from "../ipc/instances";
import type { ModpackInstallOutcomeDto } from "../ipc/modpack";
import type { TaskRecordDto } from "../ipc/tasks";
import type { IpcRecorder } from "../test/fake-bridge";
import { bootOverrides, callsOf, configDto, deferred, instanceDto, resetRecorder, settle, startedStore } from "../test/launcher";
import { NetworkMonitor } from "./online";
import { LauncherStore } from "./store";
import type { Toast } from "./toasts";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../ipc/bridge", async (orig) => (await import("../test/fake-bridge")).fakeBridge(await orig(), ipc));

const T0 = 1_720_000_000_000;
const A = "D:\\Games\\.minecraft";
const B = "E:\\HMCL\\.minecraft";
const ELSEWHERE = `这个任务发起于游戏目录 ${A}，切回去再重试`;
const UNKNOWN = "这条任务记录是旧版本留下的，不知道发起于哪个游戏目录，不能重试；需要的话重新发起";

let store: LauncherStore;
let instances: InstanceDto[];
let online = true;

async function boot(opts: { journal?: TaskRecordDto[]; gameDir?: string; network?: boolean; instances?: InstanceDto[] } = {}): Promise<void> {
  resetRecorder(ipc);
  instances = opts.instances === undefined ? [instanceDto("生存")] : opts.instances;
  bootOverrides(ipc, { instances, journal: opts.journal, config: configDto({ game_dir: opts.gameDir === undefined ? A : opts.gameDir }) });
  ipc.overrides.set("list_instances", () => instances);
  if (opts.network !== true) {
    store = await startedStore();
    return;
  }
  store = new LauncherStore({ network: new NetworkMonitor(async () => online, { navigatorOnline: () => online, target: () => window }) });
  store.start();
  await settle();
}

/** 切到 dir；那边的实例列表是 list。 */
async function switchTo(dir: string, list: InstanceDto[]): Promise<void> {
  ipc.overrides.set("set_game_directory", () => null);
  ipc.overrides.set("get_config", () => configDto({ game_dir: dir }));
  instances = list;
  await store.actions.setGameDirectory(dir);
  await settle();
  expect(store.getState().settings?.gameDir).toBe(dir);
}

/** 在当前目录里发起一次备份并让它失败，返回任务 id。 */
async function failedBackup(): Promise<string> {
  ipc.overrides.set("backup_world", () => {
    throw new IpcError("io", "磁盘满了");
  });
  await store.actions.backupWorld("生存", "新的世界", "新的世界");
  await settle();
  const task = store.getState().tasks.at(-1);
  if (task === undefined) throw new Error("没有排进任务");
  expect(task.status).toBe("failed");
  return task.id;
}

function taskOf(id: string) {
  const task = store.getState().tasks.find((t) => t.id === id);
  if (task === undefined) throw new Error(`没有任务 ${id}`);
  return task;
}

function toastOf(title: string): Toast {
  const toast = store.getState().toasts.find((t) => t.title === title);
  if (toast === undefined) throw new Error(`没有标题为「${title}」的通知`);
  return toast;
}

function installRecord(id: string, over: Partial<TaskRecordDto> = {}): TaskRecordDto {
  return {
    id,
    kind: "game",
    title: "安装 1.21.1",
    detail: "新实例「新世界」",
    instance_id: "新世界",
    status: "failed",
    progress: 0.4,
    bytes: 0,
    total_bytes: null,
    speed_bps: 0,
    attempt: 1,
    error: { code: "network_failed", message: "连接超时" },
    created_at: T0 - 60_000,
    finished_at: T0 - 1000,
    spec: { command: "install_version", args: { id: "1.21.1", loader: null, loaderVersion: null, targetName: "新世界", useTemplate: false } },
    ...over,
  };
}

const LEFTOVER = instanceDto("新世界", {
  incomplete: { kind: "install", outcome: "failed", started_at: T0, spec: { mc_version: "1.21.1", loader: null, loader_version: null }, error: { code: "network_failed", message: "连接超时" } },
});

beforeEach(async () => {
  vi.useFakeTimers();
  vi.setSystemTime(T0);
  online = true;
  await boot();
});

afterEach(() => {
  store.stop();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("入队记下游戏目录", () => {
  it("任务带着入队那一刻的游戏目录，命令参数多一个 gameDir；写进任务日志的记录带 game_dir", async () => {
    const id = await failedBackup();
    expect(taskOf(id).gameDir).toBe(A);
    expect(callsOf(ipc, "backup_world")).toEqual([{ id: "生存", world: "新的世界", taskId: `${id}#1`, gameDir: A }]);
    await vi.advanceTimersByTimeAsync(TASK_JOURNAL_DEBOUNCE_MS);
    const saved = callsOf(ipc, "save_task_journal").at(-1);
    expect((saved?.tasks as TaskRecordDto[]).find((t) => t.id === id)?.game_dir).toBe(A);
  });
});

describe("切到别的游戏目录之后", () => {
  it("单条重试（任务行与失败通知的都是它）拒绝并写明发起于哪个目录，不发命令、任务原样留在失败", async () => {
    const id = await failedBackup();
    const toast = toastOf("备份「新的世界」没有完成");
    await switchTo(B, [instanceDto("生存")]);
    expect(() => store.actions.retryTask(id)).toThrow(ELSEWHERE);
    const action = toast.action;
    if (action === undefined || !("run" in action)) throw new Error("失败通知上没有重试");
    await expect(Promise.resolve().then(() => action.run())).rejects.toThrow(ELSEWHERE);
    await settle();
    expect(callsOf(ipc, "backup_world")).toHaveLength(1);
    expect(taskOf(id)).toMatchObject({ status: "failed", attempt: 1, error: { code: "io" } });
  });

  it("「全部重试」不收它；断网恢复的通知不数它。与游戏目录无关的 Java 下载照常收、照常数", async () => {
    store.stop();
    vi.spyOn(navigator, "onLine", "get").mockImplementation(() => online);
    await boot({ network: true });
    const backup = await failedBackup();
    await switchTo(B, [instanceDto("生存")]);
    ipc.overrides.set("install_java", () => {
      throw new IpcError("java_failed", "网络连接失败", { network: "connect" });
    });
    online = false;
    window.dispatchEvent(new Event("offline"));
    await store.actions.installJava(21);
    await settle();

    ipc.overrides.set("install_java", () => new Promise(() => undefined));
    online = true;
    window.dispatchEvent(new Event("online"));
    const toast = toastOf("网络已恢复");
    expect([toast.body, toast.action?.label]).toEqual(["有 1 个下载失败", "全部重试"]);
    expect(store.actions.retryFailedTasks()).toBe(1);
    await settle();
    expect(store.getState().tasks.map((t) => [t.kind, t.status, t.attempt])).toEqual([
      ["world_backup", "failed", 1],
      ["java", "running", 2],
    ]);
    expect(callsOf(ipc, "backup_world")).toHaveLength(1);
    expect(taskOf(backup).status).toBe("failed");
  });

  it("切回发起时的目录（写法不同也算同一个）：照常重试，命令带的还是发起时的目录", async () => {
    const id = await failedBackup();
    await switchTo(B, [instanceDto("生存")]);
    await switchTo("d:/games/.minecraft/", [instanceDto("生存")]);
    ipc.overrides.set("backup_world", () => new Promise(() => undefined));
    store.actions.retryTask(id);
    await settle();
    expect(callsOf(ipc, "backup_world").at(-1)).toEqual({ id: "生存", world: "新的世界", taskId: `${id}#2`, gameDir: A });
    expect(taskOf(id)).toMatchObject({ status: "running", attempt: 2 });
  });

  it("失败的新建：新目录里恰好有同名的半成品，也不改排成补全它，原来那一行留着", async () => {
    store.stop();
    await boot({ journal: [installRecord("task-create", { game_dir: A })] });
    await switchTo(B, [LEFTOVER]);
    expect(() => store.actions.retryTask("task-create")).toThrow(ELSEWHERE);
    expect(store.actions.retryFailedTasks()).toBe(0);
    await settle();
    expect(callsOf(ipc, "repair_instance")).toEqual([]);
    expect(store.getState().tasks.map((t) => [t.id, t.status])).toEqual([["task-create", "failed"]]);
  });

  it("关闭时中断的新建：新目录里恰好有同名的已装好的实例，也不当它已装完撤掉这一行", async () => {
    store.stop();
    const interrupted = installRecord("task-create", { status: "running", error: null, finished_at: null, game_dir: A });
    await boot({ journal: [interrupted], gameDir: B, instances: [instanceDto("新世界")] });
    expect(() => store.actions.retryTask("task-create")).toThrow(ELSEWHERE);
    expect(store.actions.retryFailedTasks()).toBe(0);
    expect(store.getState().tasks.map((t) => [t.id, t.status, t.error?.code])).toEqual([["task-create", "failed", "interrupted"]]);
  });

  it("重试「全部更新」的预检也带发起时的目录：后端已切到 B、前端还没读到时，不读那边同名实例的更新、不当作零项做完", async () => {
    store.stop();
    const items = [
      { fileName: "sodium.jar", platform: "modrinth", projectId: "AANobbMI", modVersionId: "sodium-2", title: "Sodium", fileSize: 3 },
      { fileName: "lithium.jar", platform: "modrinth", projectId: "gvQqBUqZ", modVersionId: "lithium-2", title: "Lithium", fileSize: 3 },
    ];
    const updates = installRecord("task-updates", {
      kind: "mod",
      title: "更新 2 个 Mod",
      detail: "「生存」",
      instance_id: "生存",
      game_dir: A,
      spec: { command: "update_mods", args: { versionId: "生存", items } },
    });
    await boot({ journal: [updates] });
    // 后端此刻在 B：B 里同名的「生存」没有可更新的 Mod；带着发起时目录来问的，照后端的核对拒绝
    ipc.overrides.set("check_updates", (args) => {
      if (typeof args.gameDir === "string" && args.gameDir !== B) throw new IpcError("game_dir_changed", "当前游戏目录已经换了", { expected: args.gameDir, current: B });
      return [];
    });
    store.actions.retryTask("task-updates");
    await settle();
    expect(callsOf(ipc, "check_updates")).toEqual([{ versionId: "生存", gameDir: A }]);
    expect(callsOf(ipc, "update_mod")).toEqual([]);
    expect(taskOf("task-updates")).toMatchObject({ status: "failed", attempt: 2, error: { code: "game_dir_changed" } });
  });
});

describe("旧版本留下的任务日志", () => {
  it("没有 game_dir 的记录读成不知道目录：单条重试拒绝并说明、批量不收；不拿此刻的目录去补，写回时仍是 null", async () => {
    store.stop();
    const backup: TaskRecordDto = { ...installRecord("task-backup"), kind: "world_backup", title: "备份「新的世界」", instance_id: "生存", spec: { command: "backup_world", args: { id: "生存", world: "新的世界" } } };
    await boot({ journal: [installRecord("task-create"), backup] });
    expect(store.getState().tasks.map((t) => [t.id, t.gameDir])).toEqual([
      ["task-create", null],
      ["task-backup", null],
    ]);
    expect(() => store.actions.retryTask("task-create")).toThrow(UNKNOWN);
    expect(() => store.actions.retryTask("task-backup")).toThrow(UNKNOWN);
    expect(store.actions.retryFailedTasks()).toBe(0);
    // 读回的都已结束、没有变化就不写回：借一次入队触发写回
    ipc.overrides.set("install_java", () => new Promise(() => undefined));
    await store.actions.installJava(21);
    await vi.advanceTimersByTimeAsync(TASK_JOURNAL_DEBOUNCE_MS);
    const saved = callsOf(ipc, "save_task_journal").at(-1)?.tasks as TaskRecordDto[];
    expect(saved.map((t) => [t.kind, t.game_dir])).toEqual([
      ["game", null],
      ["world_backup", null],
      ["java", A],
    ]);
    expect(callsOf(ipc, "install_version")).toEqual([]);
    expect(callsOf(ipc, "backup_world")).toEqual([]);
  });

  it("旧日志里的移动：源目录就是发起时的游戏目录，按它判断；与当前目录一致时照常重试", async () => {
    store.stop();
    const move: TaskRecordDto = {
      ...installRecord("task-move"),
      kind: "instance_move",
      title: "移动「生存」",
      detail: "移到 HMCL",
      instance_id: "生存",
      spec: { command: "move_instance", args: { id: "生存", source: A, target: B } },
    };
    await boot({ journal: [move] });
    ipc.overrides.set("move_instance", () => new Promise(() => undefined));
    store.actions.retryTask("task-move");
    await settle();
    expect(callsOf(ipc, "move_instance")).toEqual([{ taskId: "task-move#2", id: "生存", source: A, target: B }]);

    store.stop();
    await boot({ journal: [move], gameDir: B });
    expect(() => store.actions.retryTask("task-move")).toThrow(ELSEWHERE);
  });

  it("旧日志里的 Java 下载与游戏目录无关：不知道目录也照常重试", async () => {
    store.stop();
    const java: TaskRecordDto = { ...installRecord("task-java"), kind: "java", title: "下载 Java 21", detail: "", instance_id: null, spec: { command: "install_java", args: { requiredMajor: 21 } } };
    await boot({ journal: [java] });
    ipc.overrides.set("install_java", () => new Promise(() => undefined));
    store.actions.retryTask("task-java");
    await settle();
    expect(callsOf(ipc, "install_java")).toEqual([{ taskId: "task-java#2", requiredMajor: 21 }]);
  });
});

describe("别的目录里同名实例的改名与补全，不碰这个目录的任务", () => {
  it("在 B 里给同名实例改名：A 里失败的任务参数与标题不跟着改，切回 A 重试的还是原来那个实例", async () => {
    const id = await failedBackup();
    await switchTo(B, [instanceDto("生存")]);
    ipc.overrides.set("rename_instance", () => {
      instances = [instanceDto("生存二")];
      return { id: "生存二" };
    });
    await store.actions.renameInstance("生存", "生存二");
    await settle();
    expect(taskOf(id)).toMatchObject({ instanceId: "生存", spec: { args: { id: "生存", world: "新的世界" } } });
    await switchTo(A, [instanceDto("生存")]);
    ipc.overrides.set("backup_world", () => new Promise(() => undefined));
    store.actions.retryTask(id);
    await settle();
    expect(callsOf(ipc, "backup_world").at(-1)).toEqual({ id: "生存", world: "新的世界", taskId: `${id}#2`, gameDir: A });
  });

  it("在 A 里改名、等后端的时候切到了 B：跟着改名的是 A 里的任务，B 里同名实例的任务与界面都不动", async () => {
    const fromA = await failedBackup();
    const renamed = deferred<{ id: string }>();
    ipc.overrides.set("rename_instance", () => renamed.promise);
    const renaming = store.actions.renameInstance("生存", "生存二");
    await settle();
    await switchTo(B, [instanceDto("生存")]);
    const fromB = await failedBackup();
    renamed.resolve({ id: "生存二" });
    expect(await renaming).toBe("生存二");
    await settle();
    expect(taskOf(fromA)).toMatchObject({ instanceId: "生存二", spec: { args: { id: "生存二", world: "新的世界" } } });
    expect(taskOf(fromB)).toMatchObject({ instanceId: "生存", spec: { args: { id: "生存", world: "新的世界" } } });
    // 外壳按 lastRename 改写历史与路由：B 里那个「生存」的页面不能被改写成 B 里并不存在的「生存二」
    expect(store.getState().lastRename).toBeNull();
    expect(store.getState().instances.map((i) => i.id)).toEqual(["生存"]);
  });

  it("在 B 里补全同名实例成功：A 里那条失败的新建不当作已被接替撤掉", async () => {
    store.stop();
    await boot({ journal: [installRecord("task-create", { game_dir: A })] });
    await switchTo(B, [instanceDto("新世界")]);
    ipc.overrides.set("repair_instance", () => ({ reinstalled: false, libraries: 0, assets: 0, natives: 0 }));
    await store.actions.repairInstance("新世界");
    await settle();
    expect(store.getState().tasks.map((t) => [t.id === "task-create" ? t.id : t.spec.command, t.status])).toEqual([
      ["task-create", "failed"],
      ["repair_instance", "done"],
    ]);
  });
});

describe("发起之后切了目录的新建、导入与补全落地", () => {
  /** 后端此刻在 B：带着别的目录来的 Mod 安装照后端的核对拒绝。 */
  function backendIn(dir: string): void {
    ipc.overrides.set("install_mod", (args) => {
      if (args.gameDir !== dir) throw new IpcError("game_dir_changed", "当前游戏目录已经换了", { expected: args.gameDir, current: dir });
      return { skipped: [] };
    });
  }

  it("新建途中切到 B：装好后不给 B 里的同名实例写封面、不固定，通知写明在哪、不带启动按钮；开局 Mod 记的是 A，后端按 A 拒绝", async () => {
    const installed = deferred<InstallResultDto>();
    ipc.overrides.set("install_version", () => installed.promise);
    await store.actions.createInstance({
      name: "新世界",
      mcVersion: "1.21.1",
      loader: "fabric",
      loaderVersion: "0.16.10",
      cover: "grass",
      useTemplate: false,
      starterMods: [{ projectId: "P7dR8mSH", modVersionId: "fapi-0115", title: "Fabric API", fileSize: 3 }],
    });
    await settle();
    await switchTo(B, [instanceDto("新世界")]);
    backendIn(B);
    installed.resolve({ id: "新世界", mc_version: "1.21.1", loader: { kind: "fabric", version: "0.16.10" }, libraries: 0, assets: 0, natives: 0, template: null });
    await settle();

    expect(callsOf(ipc, "get_instance_settings")).toEqual([]);
    expect(callsOf(ipc, "set_instance_settings")).toEqual([]);
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([]);
    const ready = toastOf("「新世界」已就绪");
    expect([ready.body, ready.action]).toEqual([`它在游戏目录 ${A} 里，当前已换到别的目录，封面与侧栏固定没有写上；切回去再启动`, undefined]);
    const starter = store.getState().tasks.find((t) => t.spec.command === "install_mods");
    expect(starter).toMatchObject({ gameDir: A, status: "failed", error: { code: "game_dir_changed" } });
    expect(callsOf(ipc, "install_mod").map((a) => a.gameDir)).toEqual([A]);
  });

  it("导入整合包途中切到 B：同样不写封面、不固定，通知写明在哪、不带启动按钮", async () => {
    const imported = deferred<ModpackInstallOutcomeDto>();
    ipc.overrides.set("install_modpack", () => imported.promise);
    await store.actions.installModpack({
      source: { kind: "file", path: "D:/包.mrpack" },
      name: "整合包",
      packName: "樱落",
      packVersion: "1.0.0",
      modCount: 3,
      cover: "grass",
      excludedOptional: [],
      mcVersion: "1.21.1",
      loader: "fabric",
      loaderVersion: "0.16.5",
    });
    await settle();
    await switchTo(B, [instanceDto("整合包")]);
    imported.resolve({ version_id: "整合包", version_dir: `${A}\versions\整合包`, modpack: null as never });
    await settle();

    expect(callsOf(ipc, "set_instance_settings")).toEqual([]);
    expect(callsOf(ipc, "set_sidebar_pins")).toEqual([]);
    const done = toastOf("「整合包」已导入");
    expect([done.body, done.action]).toEqual([`它在游戏目录 ${A} 里，当前已换到别的目录，封面与侧栏固定没有写上；切回去再启动`, undefined]);
  });

  it("修好后接着启动、补全途中切到 B：不启动 B 里的同名实例，通知写明在哪、不带启动按钮", async () => {
    const repaired = deferred<RepairResultDto>();
    ipc.overrides.set("repair_instance", () => repaired.promise);
    await store.actions.repairInstance("生存", { launchAfter: true });
    await settle();
    await switchTo(B, [instanceDto("生存")]);
    repaired.resolve({ reinstalled: false, libraries: 0, assets: 0, natives: 0 });
    await settle();

    expect(callsOf(ipc, "launch_game")).toEqual([]);
    const done = toastOf("「生存」已修复");
    expect([done.body, done.action]).toEqual([`它在游戏目录 ${A} 里，当前已换到别的目录，没有接着启动；切回去再启动`, undefined]);
  });

  it("切走又切回发起时的目录再落地：照常写封面、固定与接着启动", async () => {
    const repaired = deferred<RepairResultDto>();
    ipc.overrides.set("repair_instance", () => repaired.promise);
    ipc.overrides.set("launch_game", () => new Promise(() => undefined));
    await store.actions.repairInstance("生存", { launchAfter: true });
    await settle();
    await switchTo(B, [instanceDto("生存")]);
    await switchTo(A.toLowerCase(), [instanceDto("生存")]);
    repaired.resolve({ reinstalled: false, libraries: 0, assets: 0, natives: 0 });
    await settle();
    expect(callsOf(ipc, "launch_game").map((a) => a.versionId)).toEqual(["生存"]);
  });
});
