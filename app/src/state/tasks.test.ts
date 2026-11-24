// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TASK_INTERRUPTED_CODE } from "../copy/flows";
import { INSTALL_PHASES, type InstallPhase, type Task } from "../domain/types";
import { CORE_EVENT, type TaskRecordDto } from "../ipc/tasks";
import { APP_NAME } from "../lib/app-info";
import type { IpcRecorder } from "../test/fake-bridge";
import { bootOverrides, callsOf, cancelledError, deferred, emit, resetRecorder, settle, startedStore, type Deferred } from "../test/launcher";
import {
  applyCompositeDownload,
  applyTaskDownload,
  applyTaskPhase,
  batchRetryable,
  commandGameDir,
  compositeProgress,
  completeCompositeStep,
  dropSuperseded,
  findDuplicate,
  followMove,
  inGameDir,
  isPhasedTask,
  markDone,
  markRunning,
  modFilesBeingUpdated,
  movingKeys,
  newTask,
  PHASE_WEIGHT,
  progressTrack,
  queuePosition,
  retryBlockedReason,
  retryGameDirReason,
  retryTask,
  sharedSavesKeys,
  stageProgress,
  startComposite,
  strandedByMove,
  trimHistory,
  tasksToStart,
  type CompositeProgress,
  type DownloadSnapshot,
} from "./tasks";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../ipc/bridge", async (orig) => (await import("../test/fake-bridge")).fakeBridge(await orig(), ipc));

const T0 = 1_720_000_000_000;

/** 调度：shared 是共用存档的实例，大多数用例与它无关。 */
const start = (tasks: readonly Task[], shared: readonly string[] = []) => tasksToStart(tasks, new Set(shared));

function task(id: string, overrides: Partial<Task> = {}): Task {
  return {
    ...newTask({ gameDir: "D:/Games/.minecraft", id, kind: "java", title: `Java ${id}`, detail: "", instanceId: null, spec: { command: "install_java", args: { requiredMajor: Number(id.replace(/\D/g, "")) } } }, T0),
    ...overrides,
  };
}

function record(id: string, status: TaskRecordDto["status"], overrides: Partial<TaskRecordDto> = {}): TaskRecordDto {
  return {
    id,
    kind: "java",
    title: `Java ${id}`,
    detail: "Mojang 运行时 · 手动",
    instance_id: null,
    status,
    progress: 0.4,
    bytes: 4096,
    total_bytes: 10240,
    speed_bps: 1_000_000,
    attempt: 1,
    error: null,
    created_at: T0 - 60_000,
    finished_at: status === "queued" || status === "running" ? null : T0 - 1000,
    game_dir: "D:/Games/.minecraft",
    spec: { command: "install_java", args: { requiredMajor: 21 } },
    ...overrides,
  };
}

describe("下载队列：纯函数", () => {
  it("进度条标识：同批次不变，换批次或换尝试即变", () => {
    const a = task("t", { status: "running", totalBytes: 1000 });
    expect(progressTrack(a)).toBe(progressTrack({ ...a, bytes: 500, progress: 0.5 }));
    expect(progressTrack(a)).not.toBe(progressTrack({ ...a, totalBytes: 4000 }));
    expect(progressTrack(a)).not.toBe(progressTrack({ ...a, attempt: 2 }));
    expect(progressTrack({ ...a, totalBytes: 0 })).toBe("t#1:0:0");
    expect(progressTrack({ ...a, totalBytes: null })).toBe("t#1:count:0");
  });

  it("进度条标识：声明大小都未知的前后两批按文件数分开", () => {
    // 真实的 Fabric 安装：先是 1 个文件的版本 JSON，接着 75 个文件的本体与库，加载器的库没声明大小，两批的总字节都是未知
    const snap = (total: number, finished: number, bytes: number) => ({ total, finished, bytes, totalBytes: null, transferred: bytes, speed: 0 });
    const json = applyTaskDownload([task("t1", { status: "running" })], "t1#1", snap(1, 1, 35_014));
    expect(json[0].progress).toBe(1);
    const libraries = applyTaskDownload(json, "t1#1", snap(75, 0, 0));
    expect(libraries[0].progress).toBe(0);
    expect(progressTrack(libraries[0])).not.toBe(progressTrack(json[0]));
    const more = applyTaskDownload(libraries, "t1#1", snap(75, 30, 40_000_000));
    expect(more[0].progress).toBeCloseTo(0.4, 10);
    expect(progressTrack(more[0])).toBe(progressTrack(libraries[0]));
  });

  it("新批次的第一条快照已经下了比上一批多的字节：文件数变了也从头算起", () => {
    // 进度事件有节流，新批次的 0 字节那一条可能根本发不出来
    const snap = (total: number, finished: number, bytes: number) => ({ total, finished, bytes, totalBytes: null, transferred: bytes, speed: 0 });
    const json = applyTaskDownload([task("t1", { status: "running" })], "t1#1", snap(1, 1, 35_014));
    const libraries = applyTaskDownload(json, "t1#1", snap(75, 15, 5_000_000));
    expect(libraries[0].progress).toBeCloseTo(0.2, 10);
    expect(libraries[0].doneBytes).toBe(5_035_014);
    expect(progressTrack(libraries[0])).not.toBe(progressTrack(json[0]));
  });

  it("调度按数组顺序取最靠前的 queued，同时最多 3 个在跑", () => {
    const tasks = [task("t1"), task("t2", { status: "running" }), task("t3"), task("t4"), task("t5")];
    expect(start(tasks).map((t) => t.id)).toEqual(["t1", "t3"]);
    expect(queuePosition(tasks, "t1")).toBe(1);
    expect(queuePosition(tasks, "t4")).toBe(3);
    expect(queuePosition(tasks, "t2")).toBeNull();
  });

  describe("同一实例上写整个目录的任务（新建、导入、补全）不与这个实例上的其它任务同时跑", () => {
    const repair = (id: string, instanceId: string, overrides: Partial<Task> = {}) =>
      task(id, { kind: "repair", instanceId, spec: { command: "repair_instance", args: { id: instanceId } }, ...overrides });
    const mods = (id: string, instanceId: string, overrides: Partial<Task> = {}) =>
      task(id, { kind: "mod", instanceId, spec: { command: "install_mods", args: { versionId: instanceId, platform: "modrinth", projectId: id, items: [] } }, ...overrides });

    it("装 Mod 在跑时，同一实例的补全留在队里，别的实例与不挂实例的任务照常开始", () => {
      const tasks = [mods("m1", "生存", { status: "running" }), repair("r1", "生存"), repair("r2", "创造"), task("j21")];
      expect(start(tasks).map((t) => t.id)).toEqual(["r2", "j21"]);
    });

    it("实例 id 不分大小写：补全「Survival」在跑时，「survival」上的装 Mod 等着", () => {
      const tasks = [repair("r1", "Survival", { status: "running" }), mods("m1", "survival")];
      expect(start(tasks)).toEqual([]);
    });

    it("先到先下：排在前面、还在等的补全挡住同一实例后来的装 Mod，即使装 Mod 彼此可以同时跑", () => {
      const tasks = [mods("m1", "生存", { status: "running" }), repair("r1", "生存"), mods("m2", "生存")];
      expect(start(tasks)).toEqual([]);
      const afterFirst = tasks.map((t) => (t.id === "m1" ? { ...t, status: "done" as const, finishedAt: T0 } : t));
      expect(start(afterFirst).map((t) => t.id)).toEqual(["r1"]);
    });

    it("同一实例上的装 Mod 与装资源彼此兼容，可以同时开始", () => {
      const tasks = [mods("m1", "生存", { status: "running" }), mods("m2", "生存"), mods("m3", "生存")];
      expect(start(tasks).map((t) => t.id)).toEqual(["m2", "m3"]);
    });

    it("同一次调度里也互斥：空位够时，排队的补全与它后面同一实例的装 Mod 只开始前一个", () => {
      const tasks = [repair("r1", "生存"), mods("m1", "生存"), task("j21")];
      expect(start(tasks).map((t) => t.id)).toEqual(["r1", "j21"]);
    });
  });

  describe("同一实例上动存档的任务也不同时跑：后端的备份与备份互斥，导出整合包要等备份、恢复做完", () => {
    const world = (id: string, instanceId: string, command: "backup_world" | "export_world" | "restore_world_backup", overrides: Partial<Task> = {}) =>
      task(id, { kind: command === "restore_world_backup" ? "world_restore" : "world_backup", instanceId, spec: { command, args: { id: instanceId, world: id } }, ...overrides });
    const pack = (id: string, instanceId: string, overrides: Partial<Task> = {}) =>
      task(id, { kind: "modpack_export", instanceId, spec: { command: "export_modpack", args: { id: instanceId, options: { name: id, extras: [] } } }, ...overrides });
    const mods = (id: string, instanceId: string) =>
      task(id, { kind: "mod", instanceId, spec: { command: "install_mods", args: { versionId: instanceId, platform: "modrinth", projectId: id, items: [] } } });

    const resource = (id: string, instanceId: string, resourceType: string, overrides: Partial<Task> = {}) =>
      task(id, { kind: "datapack", instanceId, spec: { command: "install_resource", args: { versionId: instanceId, resourceType, world: "w1", platform: "modrinth", projectId: id, fileVersionId: "v1" } }, ...overrides });

    it("装数据包写的是存档：等同一实例上的备份、恢复做完，备份也等它", () => {
      expect(start([world("w1", "生存", "backup_world", { status: "running" }), resource("d1", "生存", "data_pack")])).toEqual([]);
      expect(start([resource("d1", "生存", "data_pack"), world("w1", "生存", "restore_world_backup")]).map((t) => t.id)).toEqual(["d1"]);
    });

    it("装数据包在共用存档的两个实例之间也排开，隔离的实例照常开始", () => {
      const tasks = [world("w1", "生存", "backup_world", { status: "running" }), resource("d1", "创造", "data_pack")];
      expect(start(tasks, ["生存", "创造"])).toEqual([]);
      expect(start(tasks, ["生存"]).map((t) => t.id)).toEqual(["d1"]);
    });

    it("装数据包与导出整合包同样排开：导出要读这份存档", () => {
      expect(start([pack("p1", "生存", { status: "running" }), resource("d1", "生存", "data_pack")])).toEqual([]);
      expect(start([resource("d1", "生存", "data_pack", { status: "running" }), pack("p1", "创造")], ["生存", "创造"])).toEqual([]);
    });

    it("资源包与光影不写存档，不等存档任务", () => {
      const tasks = [world("w1", "生存", "backup_world", { status: "running" }), resource("r1", "生存", "resource_pack"), resource("s1", "生存", "shader")];
      expect(start(tasks).map((t) => t.id)).toEqual(["r1", "s1"]);
    });

    it("同一实例上两个存档的备份一个一个来，别的实例的备份照常开始；实例 id 不分大小写", () => {
      const tasks = [world("w1", "Survival", "backup_world"), world("w2", "survival", "backup_world"), world("w3", "创造", "backup_world")];
      expect(start(tasks).map((t) => t.id)).toEqual(["w1", "w3"]);
    });

    it("恢复与导出存档同样排着，前一个结束就轮到它", () => {
      const running = world("w1", "生存", "restore_world_backup", { status: "running" });
      const tasks = [running, world("w2", "生存", "export_world")];
      expect(start(tasks)).toEqual([]);
      expect(start([{ ...running, status: "done", finishedAt: T0 }, tasks[1]]).map((t) => t.id)).toEqual(["w2"]);
    });

    it("备份在跑时同一实例的导出整合包等着，反过来排队的恢复也等导出读完", () => {
      expect(start([world("w1", "生存", "backup_world", { status: "running" }), pack("p1", "生存")])).toEqual([]);
      expect(start([pack("p1", "生存", { status: "running" }), world("w1", "生存", "restore_world_backup")])).toEqual([]);
    });

    it("备份只与 Mod 改动兼容：同一实例的装 Mod 与备份一起开始，两个导出整合包也一起开始", () => {
      expect(start([world("w1", "生存", "backup_world"), mods("m1", "生存")]).map((t) => t.id)).toEqual(["w1", "m1"]);
      expect(start([pack("p1", "生存"), pack("p2", "生存")]).map((t) => t.id)).toEqual(["p1", "p2"]);
    });

    describe("不隔离的实例共用一份存档，后端跨实例拒绝的组合同样排开", () => {
      const shared = ["甲", "乙"];

      it("甲在备份时乙的导出整合包等着，备份结束才开始", () => {
        const running = world("w1", "甲", "backup_world", { status: "running" });
        expect(start([running, pack("p1", "乙")], shared)).toEqual([]);
        expect(start([{ ...running, status: "done", finishedAt: T0 }, pack("p1", "乙")], shared).map((t) => t.id)).toEqual(["p1"]);
      });

      it("同时排进来：先到的开始，后到的等它；恢复与导出存档也一样，反过来排队的恢复等导出读完", () => {
        expect(start([world("w1", "甲", "backup_world"), pack("p1", "乙")], shared).map((t) => t.id)).toEqual(["w1"]);
        expect(start([world("w1", "甲", "export_world", { status: "running" }), pack("p1", "乙")], shared)).toEqual([]);
        expect(start([pack("p1", "甲", { status: "running" }), world("w1", "乙", "restore_world_backup")], shared)).toEqual([]);
      });

      it("键不分大小写，与后端的实例锁一致", () => {
        expect(start([world("w1", "Survival", "backup_world", { status: "running" }), pack("p1", "creative")], ["survival", "creative"])).toEqual([]);
      });

      it("只要有一边隔离就不相干，照常开始", () => {
        expect(start([world("w1", "甲", "backup_world", { status: "running" }), pack("p1", "丙")], shared).map((t) => t.id)).toEqual(["p1"]);
        expect(start([world("w1", "丙", "backup_world", { status: "running" }), pack("p1", "甲")], shared).map((t) => t.id)).toEqual(["p1"]);
      });

      it("跨实例只排开存档任务与导出整合包：两个备份、新建与补全都照常开始", () => {
        expect(start([world("w1", "甲", "backup_world", { status: "running" }), world("w2", "乙", "backup_world")], shared).map((t) => t.id)).toEqual(["w2"]);
        expect(start([world("w1", "甲", "backup_world", { status: "running" }), mods("m1", "乙")], shared).map((t) => t.id)).toEqual(["m1"]);
      });
    });
  });

  it("共用存档的实例：只算明确不隔离的，隔离结果读不出来的不算；键一律小写", () => {
    expect([...sharedSavesKeys([{ id: "Survival", isolated: false }, { id: "创造", isolated: true }, { id: "坏的", isolated: null }])]).toEqual(["survival"]);
  });

  it("重试只对失败的任务，换下一次尝试并排到队尾", () => {
    const tasks = [task("t1", { status: "failed", attempt: 1, error: { code: "network_failed", message: "x" }, finishedAt: T0 }), task("t2")];
    const retried = retryTask(tasks, "t1");
    expect(retried.map((t) => t.id)).toEqual(["t2", "t1"]);
    expect(retried[1]).toMatchObject({ status: "queued", attempt: 2, error: null, progress: 0, finishedAt: null });
    expect(() => retryTask(tasks, "t2")).toThrow("只有失败的任务能重试");
  });

  it("去重：同一资源装到同一实例算同一件事，实例名不分大小写；已结束的不算", () => {
    const spec = (versionId: string) => ({ command: "install_mods", args: { versionId, platform: "modrinth", projectId: "AANobbMI", items: [] } });
    const tasks = [task("t1", { spec: spec("Fabric") }), task("t2", { status: "done", finishedAt: T0, spec: spec("Other") })];
    expect(findDuplicate(tasks, { command: "install_resource", args: { versionId: "fabric", platform: "modrinth", projectId: "AANobbMI" } })?.id).toBe("t1");
    expect(findDuplicate(tasks, spec("other"))).toBeUndefined();
  });

  it("历史只留最近 100 条已结束的任务，进行中的不受影响", () => {
    const finished = Array.from({ length: 101 }, (_, i) => task(`d${i}`, { status: "done", finishedAt: T0 + i }));
    const active = task("live", { status: "running" });
    const kept = trimHistory([active, ...finished]);
    expect(kept).toHaveLength(101);
    expect(kept.some((t) => t.id === "d0")).toBe(false);
    expect(kept.some((t) => t.id === "d100")).toBe(true);
    expect(kept[0].id).toBe("live");
    expect(() => trimHistory([...finished, task("bad", { status: "failed", finishedAt: null })])).toThrow("没有结束时刻");
  });

  it("下载进度按批次如实显示：批次内只进不退，新批次从头算起", () => {
    const running = [task("t1", { status: "running" })];
    const first = applyTaskDownload(running, "t1#1", { total: 10, finished: 8, bytes: 800, totalBytes: 1000, transferred: 0, speed: 5 });
    expect(first[0]).toMatchObject({ bytes: 800, totalBytes: 1000, speedBps: 5 });
    expect(first[0].progress).toBeCloseTo(0.8, 10);
    const more = applyTaskDownload(first, "t1#1", { total: 10, finished: 9, bytes: 900, totalBytes: 1000, transferred: 0, speed: 5 });
    expect(more[0].progress).toBeCloseTo(0.9, 10);
    const nextBatch = applyTaskDownload(more, "t1#1", { total: 40, finished: 4, bytes: 100, totalBytes: 1000, transferred: 0, speed: 5 });
    expect(nextBatch[0].progress).toBeCloseTo(0.1, 10);
    // 别的尝试的进度不认
    expect(applyTaskDownload(first, "t1#2", { total: 1, finished: 1, bytes: 1, totalBytes: 1, transferred: 0, speed: 1 })).toEqual(first);
  });

  it("累计字节跨批次往上加：批次内按增量计，新批次从它自己的已下字节算起", () => {
    const running = [task("t1", { status: "running" })];
    const a = applyTaskDownload(running, "t1#1", { total: 10, finished: 2, bytes: 200, totalBytes: 1000, transferred: 0, speed: 5 });
    const b = applyTaskDownload(a, "t1#1", { total: 10, finished: 10, bytes: 1000, totalBytes: 1000, transferred: 0, speed: 5 });
    expect(b[0].doneBytes).toBe(1000);
    // 第二批：资源文件 500 字节
    const c = applyTaskDownload(b, "t1#1", { total: 3, finished: 1, bytes: 120, totalBytes: 500, transferred: 0, speed: 5 });
    expect(c[0]).toMatchObject({ bytes: 120, doneBytes: 1120 });
    const d = applyTaskDownload(c, "t1#1", { total: 3, finished: 3, bytes: 500, totalBytes: 500, transferred: 0, speed: 5 });
    expect(d[0]).toMatchObject({ bytes: 500, doneBytes: 1500 });
    // 第三批大小与第二批相同，但已下字节变少：仍认作新批次
    const e = applyTaskDownload(d, "t1#1", { total: 3, finished: 0, bytes: 0, totalBytes: 500, transferred: 0, speed: 0 });
    expect(e[0].doneBytes).toBe(1500);
  });

  it("传输量跨批次累加：批次内按增量计，新批次从它自己的传输字节算起；跳过的文件只进累计字节", () => {
    const snap = (bytes: number, totalBytes: number | null, transferred: number) => ({ total: 10, finished: 0, bytes, totalBytes, transferred, speed: 5 });
    const a = applyTaskDownload([task("t1", { status: "running" })], "t1#1", snap(600, 1000, 100));
    expect(a[0]).toMatchObject({ doneBytes: 600, transferredBytes: 100, batchTransferred: 100 });
    const b = applyTaskDownload(a, "t1#1", snap(1000, 1000, 150));
    expect(b[0]).toMatchObject({ doneBytes: 1000, transferredBytes: 150, batchTransferred: 150 });
    // 第二批：大小不同，从 0 重新计起
    const c = applyTaskDownload(b, "t1#1", snap(120, 500, 20));
    expect(c[0]).toMatchObject({ transferredBytes: 170, batchTransferred: 20 });
    // 第三批大小与第二批相同，传输字节变少：还是新批次
    const d = applyTaskDownload(c, "t1#1", snap(5, 500, 5));
    expect(d[0]).toMatchObject({ transferredBytes: 175, batchTransferred: 5 });
    // 大小都未知的批次同样靠传输字节变少认出来
    const e = applyTaskDownload(applyTaskDownload(d, "t1#1", snap(800, null, 800)), "t1#1", snap(7, null, 7));
    expect(e[0].transferredBytes).toBe(982);
  });

  it("重传时有效字节退回、传输字节还在涨：还是同一批，已传的不重复算", () => {
    const snap = (bytes: number, transferred: number) => ({ total: 1, finished: 0, bytes, totalBytes: 1000, transferred, speed: 5 });
    const a = applyTaskDownload([task("t1", { status: "running" })], "t1#1", snap(1000, 1000));
    // 校验不过，整份退回后重下：网上又走了 200 字节
    const b = applyTaskDownload(a, "t1#1", snap(200, 1200));
    expect(b[0]).toMatchObject({ bytes: 200, transferredBytes: 1200 });
    const c = applyTaskDownload(b, "t1#1", snap(1000, 2000));
    expect(c[0].transferredBytes).toBe(2000);
  });

  it("读回的旧记录没有传输量，不会再运行；运行中却没有是状态层的契约错误", () => {
    const broken = task("t1", { status: "running", transferredBytes: null });
    expect(() => applyTaskDownload([broken], "t1#1", { total: 1, finished: 0, bytes: 1, totalBytes: 2, transferred: 1, speed: 1 })).toThrow("没有传输量");
  });

  describe("分步任务（新建、补全实例）的整体进度", () => {
    // 原版新建的四步，权重 1 / 7 / 11 / 1，共 20
    const VANILLA: readonly InstallPhase[] = ["metadata", "libraries", "assets", "natives"];
    const snap = (total: number, finished: number, bytes: number, totalBytes: number | null): DownloadSnapshot => ({ total, finished, bytes, totalBytes, transferred: bytes, speed: 0 });

    it("三批各自从 0 跑到 100%，整体进度只进不退；上一步晚到的收尾快照不算进新的一步", () => {
      let tasks = [task("t1", { status: "running" })];
      const seen: number[] = [];
      const tracks = new Set<string>();
      const record = () => {
        seen.push(tasks[0].progress);
        tracks.add(progressTrack(tasks[0]));
      };
      const phase = (index: number) => {
        tasks = applyTaskPhase(tasks, "t1#1", VANILLA, index);
        record();
      };
      const download = (...args: Parameters<typeof snap>) => {
        tasks = applyTaskDownload(tasks, "t1#1", snap(...args));
        record();
      };
      phase(0);
      // 版本 JSON：一个没声明大小的文件
      download(1, 0, 0, null);
      download(1, 1, 35_014, null);
      phase(1);
      download(40, 0, 0, 50e6);
      download(40, 20, 25e6, 50e6);
      download(40, 40, 50e6, 50e6);
      phase(2);
      // 资源那一步已经开始，库那一批的收尾快照这时才到
      download(40, 40, 50e6, 50e6);
      download(3000, 0, 0, 124e6);
      download(3000, 600, 24.8e6, 124e6);
      download(3000, 3000, 124e6, 124e6);
      phase(3);
      const expected = [0, 0, 0.05, 0.05, 0.05, 0.225, 0.4, 0.4, 0.4, 0.4, 0.51, 0.95, 0.95];
      expect(seen).toHaveLength(expected.length);
      seen.forEach((progress, i) => expect(progress, `第 ${i + 1} 条`).toBeCloseTo(expected[i], 10));
      expect(seen.every((progress, i) => i === 0 || progress >= seen[i - 1])).toBe(true);
      expect([...tracks]).toEqual(["t1#1:phased"]);
      expect(tasks[0].doneBytes).toBe(174_035_014);
      expect(tasks[0].phase).toEqual({ plan: VANILLA, index: 3, batch: null, fraction: 0 });
      expect(markDone(tasks, "t1", 1, T0)[0].progress).toBe(1);
    });

    it("一出现就已下完的批次不算：文件全都现成，那一步的份额在下一步开始时补上，字节照样记账", () => {
      let tasks = applyTaskPhase([task("t1", { status: "running" })], "t1#1", VANILLA, 1);
      expect(tasks[0].progress).toBeCloseTo(0.05, 10);
      tasks = applyTaskDownload(tasks, "t1#1", snap(40, 40, 50e6, 50e6));
      expect(tasks[0].progress).toBeCloseTo(0.05, 10);
      expect(tasks[0].phase).toEqual({ plan: VANILLA, index: 1, batch: null, fraction: 0 });
      expect(tasks[0].doneBytes).toBe(50e6);
      // 边界：一个文件都没有的空批次同样不算
      tasks = applyTaskDownload(tasks, "t1#1", snap(0, 0, 0, 0));
      expect(tasks[0].phase).toEqual({ plan: VANILLA, index: 1, batch: null, fraction: 0 });
      tasks = applyTaskPhase(tasks, "t1#1", VANILLA, 2);
      expect(tasks[0].progress).toBeCloseTo(0.4, 10);
    });

    it("步骤往回走、报两遍、越界或改了已走过的步骤是后端的契约错误；别的尝试、已结束的任务不认", () => {
      const at2 = applyTaskPhase([task("t1", { status: "running" })], "t1#1", VANILLA, 2);
      expect(() => applyTaskPhase(at2, "t1#1", VANILLA, 1)).toThrow("已在第 2 步");
      expect(() => applyTaskPhase(at2, "t1#1", VANILLA, 2)).toThrow("已在第 2 步");
      // 改写过的表同样不能停在原地或往回走
      expect(() => applyTaskPhase(at2, "t1#1", ["metadata", "libraries", "assets", "java", "verify"], 2)).toThrow("已在第 2 步");
      const fresh = [task("t1", { status: "running" })];
      expect(() => applyTaskPhase(fresh, "t1#1", ["metadata", "libraries"], 2)).toThrow("只有 2 步");
      expect(() => applyTaskPhase(fresh, "t1#1", ["metadata", "libraries"], -1)).toThrow("只有 2 步");
      expect(() => applyTaskPhase(fresh, "t1#1", ["metadata", "libraries"], 0.5)).toThrow("只有 2 步");
      // 改写成只到已走过那几步的表，下一步就越界了
      expect(() => applyTaskPhase(at2, "t1#1", ["metadata", "libraries", "assets"], 3)).toThrow("只有 3 步");
      // 已走过的是第 0 到第 2 步：换掉头一步、在中间插一步、换掉当前这一步都不行
      expect(() => applyTaskPhase(at2, "t1#1", ["loader_profile", "libraries", "assets", "natives"], 3)).toThrow("已走过的步骤变了：metadata,libraries,assets -> loader_profile,libraries,assets");
      expect(() => applyTaskPhase(at2, "t1#1", ["metadata", "loader_profile", "libraries", "assets", "natives"], 3)).toThrow("已走过的步骤变了");
      expect(() => applyTaskPhase(at2, "t1#1", ["metadata", "libraries", "java", "natives"], 3)).toThrow("已走过的步骤变了：metadata,libraries,assets -> metadata,libraries,java");
      expect(at2[0].phase).toEqual({ plan: VANILLA, index: 2, batch: null, fraction: 0 });
      // 边界：紧接着要进的那一步还没走到，可以换
      const revised: readonly InstallPhase[] = ["metadata", "libraries", "assets", "java", "verify"];
      expect(applyTaskPhase(at2, "t1#1", revised, 3)[0].phase).toEqual({ plan: revised, index: 3, batch: null, fraction: 0 });
      expect(applyTaskPhase(at2, "t1#2", VANILLA, 3)).toEqual(at2);
      const done = markDone(at2, "t1", 1, T0);
      expect(applyTaskPhase(done, "t1#1", VANILLA, 3)).toEqual(done);
    });

    it("导入整合包读完包后改写后面的步骤：已走过的原样不变，接受改写，之后按新表折算", () => {
      // 平台整合包开工时预排的表，权重 2 / 1 / 1 / 7 / 11 / 1 / 12 / 2，共 37
      const prelude: readonly InstallPhase[] = ["modpack_download", "modpack_read", "metadata", "libraries", "assets", "natives", "modpack_files", "modpack_overrides"];
      // 读完包：Fabric、有远程文件、没有覆盖文件、套默认设置，权重 2 / 1 / 1 / 7 / 11 / 1 / 1 / 3 / 12 / 1，共 40
      const exact: readonly InstallPhase[] = ["modpack_download", "modpack_read", "metadata", "libraries", "assets", "natives", "loader_profile", "loader_libraries", "modpack_files", "template"];
      let tasks = applyTaskPhase([task("t1", { status: "running" })], "t1#1", prelude, 0);
      expect(tasks[0].progress).toBe(0);
      tasks = applyTaskDownload(tasks, "t1#1", snap(1, 0, 0, 80e6));
      tasks = applyTaskDownload(tasks, "t1#1", snap(1, 1, 80e6, 80e6));
      expect(tasks[0].progress).toBeCloseTo(2 / 37, 10);
      tasks = applyTaskPhase(tasks, "t1#1", prelude, 1);
      const before = tasks[0].progress;
      expect(before).toBeCloseTo(2 / 37, 10);
      tasks = applyTaskPhase(tasks, "t1#1", exact, 2);
      expect(tasks[0].phase).toEqual({ plan: exact, index: 2, batch: null, fraction: 0 });
      expect(tasks[0].progress).toBeCloseTo(3 / 40, 10);
      expect(tasks[0].progress).toBeGreaterThan(before);
      tasks = applyTaskPhase(tasks, "t1#1", exact, 6);
      expect(tasks[0].progress).toBeCloseTo(23 / 40, 10);
      // 改写之后，改写进来的步骤也算已走过
      expect(() => applyTaskPhase(tasks, "t1#1", prelude, 7)).toThrow("已走过的步骤变了");
    });

    it("改写成更重的表、新起点比已走到的低：整体进度停在原处，等新表追上来", () => {
      // 改写后的表权重 1 / 7 / 11 / 1 / 4 / 1 / 3 / 6 / 2，共 36
      const heavier: readonly InstallPhase[] = ["metadata", "libraries", "assets", "natives", "java", "loader_installer", "loader_libraries", "loader_processors", "verify"];
      let tasks = applyTaskPhase([task("t1", { status: "running" })], "t1#1", VANILLA, 1);
      tasks = applyTaskDownload(tasks, "t1#1", snap(40, 0, 0, 50e6));
      tasks = applyTaskDownload(tasks, "t1#1", snap(40, 40, 50e6, 50e6));
      expect(tasks[0].progress).toBeCloseTo(0.4, 10);
      // 新表里资源那一步从 8 / 36 起
      tasks = applyTaskPhase(tasks, "t1#1", heavier, 2);
      expect(tasks[0].progress).toBeCloseTo(0.4, 10);
      tasks = applyTaskDownload(tasks, "t1#1", snap(3000, 1500, 62e6, 124e6));
      expect(tasks[0].phase).toMatchObject({ index: 2, fraction: 0.5 });
      expect(tasks[0].progress).toBeCloseTo(0.4, 10);
      tasks = applyTaskDownload(tasks, "t1#1", snap(3000, 3000, 124e6, 124e6));
      expect(tasks[0].progress).toBeCloseTo(19 / 36, 10);
    });

    describe("阶段进度", () => {
      it("fraction：进新一步为 0；认下批次后取这条快照的比例，同一批内只进不退；上一步晚到的不算；同一步换了一批取新批次的比例", () => {
        let tasks = applyTaskPhase([task("t1", { status: "running" })], "t1#1", VANILLA, 0);
        const fraction = () => {
          const phase = tasks[0].phase;
          if (phase === null) throw new Error("任务没有步骤");
          return phase.fraction;
        };
        const download = (...args: Parameters<typeof snap>) => {
          tasks = applyTaskDownload(tasks, "t1#1", snap(...args));
          return fraction();
        };
        expect(fraction()).toBe(0);
        // 版本 JSON：没声明大小，按文件数
        expect(download(1, 0, 0, null)).toBe(0);
        expect(download(1, 1, 35_014, null)).toBe(1);
        tasks = applyTaskPhase(tasks, "t1#1", VANILLA, 1);
        expect(tasks[0].phase).toEqual({ plan: VANILLA, index: 1, batch: null, fraction: 0 });
        // 上一步晚到的收尾快照
        expect(download(1, 1, 35_014, null)).toBe(0);
        expect(tasks[0].phase).toMatchObject({ batch: null });
        expect(download(40, 16, 20e6, 50e6)).toBeCloseTo(0.4, 10);
        // 一个文件校验不过、整份退回：还是同一批，不变小
        expect(download(40, 15, 15e6, 50e6)).toBeCloseTo(0.4, 10);
        expect(download(40, 30, 40e6, 50e6)).toBeCloseTo(0.8, 10);
        tasks = applyTaskPhase(tasks, "t1#1", VANILLA, 2);
        expect(fraction()).toBe(0);
        expect(download(40, 40, 50e6, 50e6)).toBe(0);
        expect(download(3000, 2400, 99.2e6, 124e6)).toBeCloseTo(0.8, 10);
        // 这一步已认下自己的批次之后，上一步的收尾快照才到
        expect(download(40, 40, 50e6, 50e6)).toBeCloseTo(0.8, 10);
        const overall = tasks[0].progress;
        expect(overall).toBeCloseTo(0.84, 10);
        // 同一步里换了一批：总量不同、没下完，从这一批自己的比例算起，整体进度不退
        expect(download(10, 1, 0.4e6, 4e6)).toBeCloseTo(0.1, 10);
        expect(tasks[0].phase).toMatchObject({ batch: { total: 10, totalBytes: 4e6 } });
        expect(tasks[0].progress).toBeCloseTo(overall, 10);
        // 边界：换成没声明大小的一批，按文件数
        expect(download(10, 3, 0, null)).toBeCloseTo(0.3, 10);
      });

      it("stageProgress：没有步骤时按是否完成；这一步没认下批次时给不出（null）；认下了取 fraction；完成为 1 且标识不变", () => {
        expect(stageProgress(task("t1"))).toEqual({ value: 0, track: "t1#1:stage" });
        const running = task("t1", { status: "running" });
        expect(stageProgress(running)).toEqual({ value: 0, track: "t1#1:stage" });
        expect(stageProgress(task("t1", { status: "done", progress: 1, finishedAt: T0 }))).toEqual({ value: 1, track: "t1#1:stage" });
        let tasks = applyTaskPhase([running], "t1#1", VANILLA, 1);
        expect(stageProgress(tasks[0])).toEqual({ value: null, track: "t1#1:stage:1" });
        tasks = applyTaskDownload(tasks, "t1#1", snap(40, 10, 12.5e6, 50e6));
        expect(stageProgress(tasks[0])).toEqual({ value: 0.25, track: "t1#1:stage:1:50000000:40" });
        expect(stageProgress({ ...tasks[0], attempt: 2 }).track).toBe("t1#2:stage:1:50000000:40");
        // 换到下一步：上一步的比例不带过来，又回到给不出
        tasks = applyTaskPhase(tasks, "t1#1", VANILLA, 3);
        expect(stageProgress(tasks[0])).toEqual({ value: null, track: "t1#1:stage:3" });
        expect(stageProgress(markDone(tasks, "t1", 1, T0)[0])).toEqual({ value: 1, track: "t1#1:stage:3" });
        // 通常不下文件的步骤（解压本地库）认下了批次也照常给出比例
        tasks = applyTaskDownload(tasks, "t1#1", snap(4, 1, 100, null));
        const measured = stageProgress(tasks[0]);
        expect(measured).toEqual({ value: 0.25, track: "t1#1:stage:3:count:4" });
        expect(stageProgress(markDone(tasks, "t1", 1, T0)[0])).toEqual({ value: 1, track: measured.track });
      });

      it("stageProgress：认下的批次只有一个没声明大小的文件时给不出（按文件数只能是 0）；声明了大小、或不止一个文件就给得出", () => {
        const entered = applyTaskPhase([task("t1", { status: "running" })], "t1#1", ["metadata", "libraries", "assets", "natives", "java", "loader_installer"], 5);
        // 加载器安装器：一个文件，没有声明大小，下了多少字节也折不出比例
        let tasks = applyTaskDownload(entered, "t1#1", snap(1, 0, 3_500_000, null));
        expect(tasks[0].phase).toMatchObject({ batch: { total: 1, totalBytes: null }, fraction: 0 });
        expect(stageProgress(tasks[0])).toEqual({ value: null, track: "t1#1:stage:5:count:1" });
        // 完成时照常换成满格，标识不变
        expect(stageProgress(markDone(tasks, "t1", 1, T0)[0])).toEqual({ value: 1, track: "t1#1:stage:5:count:1" });
        // 边界：一个文件但声明了大小，按字节给得出
        tasks = applyTaskDownload(entered, "t1#1", snap(1, 0, 1_000_000, 4_000_000));
        expect(stageProgress(tasks[0])).toEqual({ value: 0.25, track: "t1#1:stage:5:4000000:1" });
        // 边界：没声明大小但有两个文件，按文件数给得出
        tasks = applyTaskDownload(entered, "t1#1", snap(2, 1, 0, null));
        expect(stageProgress(tasks[0])).toEqual({ value: 0.5, track: "t1#1:stage:5:count:2" });
      });

      it("stageProgress：换步即换标识；任何一种步骤没认下批次时都给不出", () => {
        let tasks = applyTaskPhase([task("t1", { status: "running" })], "t1#1", VANILLA, 0);
        const tracks = [stageProgress(tasks[0]).track];
        for (const index of [1, 2, 3]) {
          tasks = applyTaskPhase(tasks, "t1#1", VANILLA, index);
          tracks.push(stageProgress(tasks[0]).track);
        }
        expect(tracks).toEqual(["t1#1:stage:0", "t1#1:stage:1", "t1#1:stage:2", "t1#1:stage:3"]);
        for (const key of INSTALL_PHASES) {
          const at = applyTaskPhase([task("t1", { status: "running" })], "t1#1", [key], 0)[0];
          expect(stageProgress(at).value, key).toBeNull();
        }
      });
    });

    it("会报步骤的任务：新建、导入整合包、补全与移动实例，导出整合包，恢复存档备份；别的任务不报", () => {
      const of = (command: string) => task("t1", { spec: { command, args: {} } });
      expect(["install_version", "install_modpack", "repair_instance", "move_instance", "export_modpack", "restore_world_backup"].map((command) => isPhasedTask(of(command)))).toEqual([true, true, true, true, true, true]);
      // 备份与导出存档只有打包一段，不分步
      expect(["install_mod", "install_mods", "install_java", "update_mods", "export_world", "backup_world"].map((command) => isPhasedTask(of(command)))).toEqual([false, false, false, false, false, false]);
    });

    it("每一种步骤都有正的权重，导入整合包的四步也在内", () => {
      expect(Object.keys(PHASE_WEIGHT).sort()).toEqual([...INSTALL_PHASES].sort());
      for (const key of INSTALL_PHASES) expect(PHASE_WEIGHT[key], key).toBeGreaterThan(0);
      expect(PHASE_WEIGHT).toMatchObject({ modpack_download: 2, modpack_read: 1, modpack_files: 12, modpack_overrides: 2 });
    });

    it("随机的批次、现成的批次与晚到快照：整体进度只进不退，每步都落在自己那一段里", () => {
      const FORGE: readonly InstallPhase[] = ["metadata", "libraries", "assets", "natives", "java", "loader_installer", "loader_libraries", "loader_processors", "verify"];
      const weights = FORGE.map((key) => PHASE_WEIGHT[key]);
      const whole = weights.reduce((sum, w) => sum + w, 0);
      const startOf = (index: number) => weights.slice(0, index).reduce((sum, w) => sum + w, 0) / whole;
      const rand = (n: number) => Math.floor(Math.random() * n);
      for (let run = 0; run < 40; run++) {
        let tasks = [task("t1", { status: "running" })];
        let last = 0;
        let lastFinal: DownloadSnapshot | null = null;
        for (let index = 0; index < FORGE.length; index++) {
          tasks = applyTaskPhase(tasks, "t1#1", FORGE, index);
          expect(tasks[0].progress, `第 ${run} 轮进入第 ${index} 步`).toBeCloseTo(Math.max(last, startOf(index)), 10);
          last = tasks[0].progress;
          // 校验那一步连跑两批，其余步骤零到两批；各批里随机夹着上一批的收尾快照
          const batches = FORGE[index] === "verify" ? 2 : rand(3);
          for (let b = 0; b < batches; b++) {
            const total = 1 + rand(5000);
            const totalBytes = rand(3) === 0 ? null : total * (1 + rand(1_000_000));
            const ready = rand(4) === 0;
            const steps = ready ? [total] : [0, ...Array.from({ length: 1 + rand(5) }, () => rand(total + 1)).sort((x, y) => x - y), total];
            for (const finished of steps) {
              if (lastFinal !== null && rand(3) === 0) tasks = applyTaskDownload(tasks, "t1#1", lastFinal);
              const bytes = totalBytes === null ? finished * 100 : Math.round((totalBytes * finished) / total);
              tasks = applyTaskDownload(tasks, "t1#1", snap(total, finished, bytes, totalBytes));
              const progress = tasks[0].progress;
              expect(progress, `第 ${run} 轮第 ${index} 步`).toBeGreaterThanOrEqual(last);
              expect(progress, `第 ${run} 轮第 ${index} 步`).toBeLessThanOrEqual(startOf(index + 1) + 1e-12);
              last = progress;
            }
            lastFinal = snap(total, total, totalBytes === null ? total * 100 : totalBytes, totalBytes);
          }
        }
        expect(last).toBeLessThanOrEqual(1);
      }
    });
  });

  it("组合任务的权重：声明大小都已知且总和大于 0 时按字节，否则按项数", () => {
    expect(compositeProgress([100, 300, 600])).toMatchObject({ weights: [100, 300, 600], totalBytes: 1000, index: 0, batches: [null, null, null], itemBytes: [0, 0, 0] });
    expect(compositeProgress([100, null, 600])).toMatchObject({ weights: [1, 1, 1], totalBytes: null });
    expect(compositeProgress([0, 0])).toMatchObject({ weights: [1, 1], totalBytes: null });
    // 大小为 0 的一项（文件已就绪）在按字节计时只是不占进度
    expect(compositeProgress([0, 512])).toMatchObject({ weights: [0, 512], totalBytes: 512 });
    expect(compositeProgress([])).toMatchObject({ weights: [], totalBytes: null, index: 0 });
  });

  it("组合任务的进度：各项从 0 报起也按整体累计，换项不归零，只进不退", () => {
    let cursor: CompositeProgress = compositeProgress([100, 300]);
    let tasks = startComposite([task("t1", { status: "running" })], "t1#1", cursor);
    expect(tasks[0]).toMatchObject({ progress: 0, bytes: 0, totalBytes: 400 });
    const step = (item: number, bytes: number, totalBytes: number | null) => {
      const next = applyCompositeDownload(tasks, "t1#1", cursor, item, { total: 1, finished: bytes === totalBytes ? 1 : 0, bytes, totalBytes, transferred: 0, speed: 7 + item });
      if (next === null) return false;
      tasks = next.tasks;
      cursor = next.cursor;
      return true;
    };
    const finish = () => {
      const next = completeCompositeStep(tasks, "t1#1", cursor);
      tasks = next.tasks;
      cursor = next.cursor;
    };
    step(0, 50, 100);
    expect(tasks[0]).toMatchObject({ progress: 0.125, bytes: 50, totalBytes: 400, doneBytes: 50, speedBps: 7 });
    step(0, 100, 100);
    finish();
    expect(tasks[0]).toMatchObject({ progress: 0.25, bytes: 100, doneBytes: 100 });
    // 上一项最后一条进度在它的命令返回后才到：续的是它自己那一批，没有新字节，也不算进下一项
    step(0, 100, 100);
    expect(tasks[0]).toMatchObject({ progress: 0.25, bytes: 100, doneBytes: 100 });
    step(1, 0, 300);
    expect(tasks[0]).toMatchObject({ progress: 0.25, bytes: 100, doneBytes: 100, speedBps: 8 });
    step(1, 150, 300);
    expect(tasks[0]).toMatchObject({ progress: 0.625, bytes: 250, doneBytes: 250 });
    // 同一项里换了一批（总量变了、从 0 报起）：整体不退，这一批的字节接着累计
    step(1, 10, 20);
    expect(tasks[0]).toMatchObject({ progress: 0.65, bytes: 260, doneBytes: 260 });
    expect(cursor.itemBytes).toEqual([100, 160]);
    finish();
    expect(tasks[0]).toMatchObject({ progress: 1, bytes: 400 });
    // 全部做完后最后一项晚到的进度：只补字节，进度与速度不动
    expect(step(1, 20, 20)).toBe(true);
    expect(tasks[0]).toMatchObject({ progress: 1, bytes: 400, doneBytes: 270, speedBps: 8 });
    // 别的尝试的进度不认
    expect(applyCompositeDownload(tasks, "t1#2", cursor, 0, { total: 1, finished: 0, bytes: 1, totalBytes: 2, transferred: 0, speed: 1 })).toBeNull();
  });

  it("组合任务按项数折算时：总量为空，已下字节累计实际下载的", () => {
    let cursor = compositeProgress([null, 2048, 4096]);
    let tasks = startComposite([task("t1", { status: "running" })], "t1#1", cursor);
    const apply = (step: number, bytes: number, totalBytes: number) => {
      const next = applyCompositeDownload(tasks, "t1#1", cursor, step, { total: 1, finished: 0, bytes, totalBytes, transferred: 0, speed: 1 });
      if (next === null) throw new Error("这条进度应被采纳");
      tasks = next.tasks;
      cursor = next.cursor;
    };
    apply(0, 512, 1024);
    expect(tasks[0].progress).toBeCloseTo(1 / 6, 10);
    expect(tasks[0]).toMatchObject({ bytes: 512, totalBytes: null, doneBytes: 512 });
    ({ tasks, cursor } = completeCompositeStep(tasks, "t1#1", cursor));
    expect(tasks[0].progress).toBeCloseTo(1 / 3, 10);
    apply(1, 1024, 2048);
    expect(tasks[0].progress).toBeCloseTo(1 / 2, 10);
    expect(tasks[0]).toMatchObject({ bytes: 1536, totalBytes: null, doneBytes: 1536 });
  });

  /** 起一个两项的组合任务，返回按项喂进度、做完当前项的操作与当前任务。 */
  function twoItems(sizes: [number | null, number | null]) {
    let cursor = compositeProgress(sizes);
    let tasks = startComposite([task("t1", { status: "running" })], "t1#1", cursor);
    return {
      feed(step: number, bytes: number, totalBytes: number | null, finished: number, speed = 1) {
        const next = applyCompositeDownload(tasks, "t1#1", cursor, step, { total: 1, finished, bytes, totalBytes, transferred: 0, speed });
        if (next === null) throw new Error("这一次尝试在运行，进度应被采纳");
        ({ tasks, cursor } = next);
      },
      finish() {
        ({ tasks, cursor } = completeCompositeStep(tasks, "t1#1", cursor));
      },
      get task() {
        return tasks[0];
      },
      get cursor() {
        return cursor;
      },
    };
  }

  it("两项声明大小相同：后一项第一条就与上一项最后一条逐字段相同，也照常算进后一项", () => {
    const run = twoItems([100, 100]);
    run.feed(0, 100, 100, 1);
    run.finish();
    expect(run.task).toMatchObject({ progress: 0.5, bytes: 100, doneBytes: 100 });
    run.feed(1, 100, 100, 1);
    expect(run.task).toMatchObject({ progress: 1, bytes: 200, totalBytes: 200, doneBytes: 200 });
    // 上一项同样的那条快照这时才到：续它自己的批次，不重复算
    run.feed(0, 100, 100, 1);
    expect(run.task).toMatchObject({ progress: 1, bytes: 200, doneBytes: 200 });
    expect(run.cursor.itemBytes).toEqual([100, 100]);
  });

  it("两项大小都未知：后一项的已下字节比上一项多也照常采纳，按项数推进", () => {
    const run = twoItems([null, null]);
    run.feed(0, 500, null, 1);
    run.finish();
    expect(run.task).toMatchObject({ progress: 0.5, bytes: 500, totalBytes: null, doneBytes: 500 });
    run.feed(1, 600, null, 0);
    expect(run.task).toMatchObject({ progress: 0.5, bytes: 1100, doneBytes: 1100 });
    run.feed(1, 900, null, 1);
    expect(run.task).toMatchObject({ progress: 1, bytes: 1400, doneBytes: 1400 });
    // 边界：后一项从 0 报起、没有新字节
    const fresh = twoItems([null, null]);
    fresh.feed(0, 0, null, 0);
    expect(fresh.task).toMatchObject({ progress: 0, bytes: 0, doneBytes: 0 });
  });

  it("上一项的进度在后一项报过进度之后才到：只补上一项的字节，整体进度与速度都不动", () => {
    const run = twoItems([100, 300]);
    run.feed(0, 60, 100, 0, 5);
    run.finish();
    run.feed(1, 30, 300, 0, 9);
    expect(run.task).toMatchObject({ progress: 0.325, bytes: 130, doneBytes: 90, speedBps: 9 });
    run.feed(0, 100, 100, 1, 5);
    expect(run.task).toMatchObject({ progress: 0.325, bytes: 130, doneBytes: 130, speedBps: 9 });
    run.feed(1, 300, 300, 1, 9);
    expect(run.task).toMatchObject({ progress: 1, bytes: 400, doneBytes: 400 });
    expect(run.cursor.itemBytes).toEqual([100, 300]);
  });

  it("组合任务的传输量：按项分开算增量，晚到的进度不重复算，跳过的文件不算", () => {
    let cursor = compositeProgress([100, 100]);
    let tasks = startComposite([task("t1", { status: "running" })], "t1#1", cursor);
    const feed = (step: number, bytes: number, totalBytes: number, transferred: number) => {
      const next = applyCompositeDownload(tasks, "t1#1", cursor, step, { total: 1, finished: 0, bytes, totalBytes, transferred, speed: 1 });
      if (next === null) throw new Error("这一次尝试在运行，进度应被采纳");
      ({ tasks, cursor } = next);
    };
    // 第 0 项本就完整：进度走满，一个字节没传
    feed(0, 60, 100, 0);
    expect(tasks[0]).toMatchObject({ doneBytes: 60, transferredBytes: 0 });
    ({ tasks, cursor } = completeCompositeStep(tasks, "t1#1", cursor));
    feed(1, 30, 100, 30);
    expect(tasks[0]).toMatchObject({ doneBytes: 90, transferredBytes: 30 });
    // 第 0 项晚到的进度带着 40 字节：记在它自己那一批上
    feed(0, 100, 100, 40);
    expect(tasks[0]).toMatchObject({ doneBytes: 130, transferredBytes: 70 });
    feed(1, 100, 100, 100);
    expect(tasks[0]).toMatchObject({ doneBytes: 200, transferredBytes: 140 });
    // 同一项里重传：有效字节退回，传输字节接着涨，不重复算
    feed(1, 20, 100, 120);
    expect(tasks[0]).toMatchObject({ transferredBytes: 160 });
    // 同一项里换了一批：总量变了，从它自己的传输字节算起
    feed(1, 3, 50, 3);
    expect(tasks[0]).toMatchObject({ transferredBytes: 163 });
  });

  it("项序号越过当前项或越界：调用方给错了序号，直接报错", () => {
    const run = twoItems([100, 100]);
    expect(() => run.feed(1, 10, 100, 0)).toThrow("还在第 0 项，收到了第 1 项的进度");
    expect(() => run.feed(2, 10, 100, 0)).toThrow("只有 2 项，收到了第 2 项的进度");
    expect(() => run.feed(-1, 10, 100, 0)).toThrow("只有 2 项，收到了第 -1 项的进度");
    expect(() => run.feed(0.5, 10, 100, 0)).toThrow("只有 2 项");
    expect(run.task).toMatchObject({ progress: 0, doneBytes: 0 });
  });

  it("重试清掉上一次尝试的开始时刻、累计字节与传输量", () => {
    const phase = { plan: ["metadata", "libraries"] as const, index: 1, batch: null, fraction: 0 };
    const failed = task("t1", { status: "failed", startedAt: T0 + 5, doneBytes: 4096, transferredBytes: 2048, batchTransferred: 1024, phase, finishedAt: T0 + 9, error: { code: "network_failed", message: "x" } });
    expect(retryTask([failed], "t1")[0]).toMatchObject({ status: "queued", startedAt: null, doneBytes: 0, transferredBytes: 0, batchTransferred: 0, phase: null });
  });

  it("开始运行时这一次尝试的进度全部从 0 算起", () => {
    const phase = { plan: ["metadata", "libraries"] as const, index: 1, batch: { total: 3, totalBytes: 10 }, fraction: 0.9 };
    const queued = task("t1", { progress: 0.5, bytes: 9, totalBytes: 10, doneBytes: 9, transferredBytes: 9, batchTransferred: 9, phase, speedBps: 9 });
    expect(markRunning([queued], "t1", T0 + 10)[0]).toMatchObject({ status: "running", progress: 0, bytes: 0, totalBytes: null, doneBytes: 0, transferredBytes: 0, batchTransferred: 0, phase: null, speedBps: 0, startedAt: T0 + 10 });
  });

  it("重试较早记录里失败的任务：传输量从 0 算起，新进度能累加", () => {
    const old = task("t1", { status: "failed", transferredBytes: null, finishedAt: T0 + 9, error: { code: "network_failed", message: "x" } });
    const running = markRunning(retryTask([old], "t1"), "t1", T0 + 10);
    expect(running[0]).toMatchObject({ status: "running", attempt: 2, transferredBytes: 0 });
    expect(applyTaskDownload(running, "t1#2", { total: 1, finished: 0, bytes: 9, totalBytes: 10, transferred: 3, speed: 1 })[0].transferredBytes).toBe(3);
  });

  it("正在更新的 Mod 文件：单个与组合任务都算，实例 id 不分大小写，已结束的与别的实例不算", () => {
    const update = (id: string, instanceId: string, args: Record<string, unknown>, command = "update_mod", status: Task["status"] = "running") =>
      task(id, { kind: "mod", instanceId, status, spec: { command, args: { versionId: instanceId, ...args } } });
    const tasks = [
      update("a", "Survival", { fileName: "sodium-1.jar" }),
      update("b", "survival", { items: [{ fileName: "lithium-1.jar" }, { fileName: "iris-1.jar" }] }, "update_mods", "queued"),
      update("c", "SURVIVAL", { fileName: "old-1.jar" }, "update_mod", "done"),
      update("d", "Creative", { fileName: "jei-1.jar" }),
      task("e", { kind: "mod", instanceId: "Survival", status: "running", spec: { command: "install_mod", args: { versionId: "Survival" } } }),
    ];
    expect([...modFilesBeingUpdated(tasks, "SURVIVAL")].sort()).toEqual(["iris-1.jar", "lithium-1.jar", "sodium-1.jar"]);
    expect([...modFilesBeingUpdated(tasks, "Nether")]).toEqual([]);
    expect(() => modFilesBeingUpdated([update("f", "Survival", { items: [{ title: "x" }] }, "update_mods")], "Survival")).toThrow("update_mods 任务的一项缺少 fileName");
  });

  it("正在更新的 Mod 文件：任务参数形状不对时抛错，不把缺口当成「没有在更新」", () => {
    const update = (args: Record<string, unknown>, command: string) => task("g", { kind: "mod", instanceId: "Survival", status: "running", spec: { command, args: { versionId: "Survival", ...args } } });
    expect(() => modFilesBeingUpdated([update({}, "update_mods")], "Survival")).toThrow("update_mods 任务缺少 items");
    expect(() => modFilesBeingUpdated([update({ items: "sodium.jar" }, "update_mods")], "Survival")).toThrow("update_mods 任务缺少 items");
    expect(() => modFilesBeingUpdated([update({ items: [null] }, "update_mods")], "Survival")).toThrow("update_mods 任务的一项缺少 fileName");
    expect(() => modFilesBeingUpdated([update({ fileName: 42 }, "update_mod")], "Survival")).toThrow("update_mod 任务的 fileName 不是字符串：42");
    expect(() => modFilesBeingUpdated([update({ items: [{ fileName: null }] }, "update_mods")], "Survival")).toThrow("update_mods 任务的 fileName 不是字符串：null");
    // 已结束的任务不看参数
    expect([...modFilesBeingUpdated([task("h", { kind: "mod", instanceId: "Survival", status: "done", spec: { command: "update_mods", args: {} } })], "Survival")]).toEqual([]);
  });
});

describe("下载队列：发起时的游戏目录", () => {
  const A = "D:\\Games\\.minecraft";
  const B = "E:\\HMCL\\.minecraft";
  const backup = (gameDir: string | null, overrides: Partial<Task> = {}) =>
    task("b1", { kind: "world_backup", instanceId: "生存", status: "failed", error: { code: "io", message: "磁盘满了" }, spec: { command: "backup_world", args: { id: "生存", world: "w" } }, gameDir, ...overrides });
  const move = (gameDir: string | null) => task("m1", { kind: "instance_move", instanceId: "生存", status: "failed", error: { code: "io", message: "x" }, spec: { command: "move_instance", args: { id: "生存", source: A, target: B } }, gameDir });

  it("重试挡不挡：同一目录（写法不同也算）不挡；别的目录写明发起于哪里；不知道发起于哪里、当前目录还没读到都挡并各自说明", () => {
    expect(retryGameDirReason(backup(A), "d:/games/.minecraft/")).toBeNull();
    expect(retryGameDirReason(backup(A), B)).toBe(`这个任务发起于游戏目录 ${A}，切回去再重试`);
    expect(retryGameDirReason(backup(null), A)).toBe("这条任务记录是旧版本留下的，不知道发起于哪个游戏目录，不能重试；需要的话重新发起");
    expect(retryGameDirReason(backup(A), null)).toBe("还没读到当前的游戏目录，读到后再重试");
  });

  it("Java 下载与游戏目录无关，从不挡；旧日志里的移动按源目录判断，源目录不是字符串按日志坏了报错", () => {
    const java = task("j1", { status: "failed", gameDir: null });
    expect(retryGameDirReason(java, null)).toBeNull();
    expect(retryGameDirReason(java, B)).toBeNull();
    expect(retryGameDirReason(move(null), A)).toBeNull();
    expect(retryGameDirReason(move(null), B)).toBe(`这个任务发起于游戏目录 ${A}，切回去再重试`);
    expect(() => retryGameDirReason({ ...move(null), spec: { command: "move_instance", args: { id: "生存" } } }, A)).toThrow("移动任务 m1 的参数 source 应为字符串");
  });

  it("批量重试只收目录对得上的失败；Java 下载照收", () => {
    const tasks = [backup(A, { id: "same" }), backup(B, { id: "other" }), backup(null, { id: "old" }), task("j1", { status: "failed", gameDir: B })];
    expect(tasks.filter((t) => batchRetryable(t, A, new Set())).map((t) => t.id)).toEqual(["same", "j1"]);
    expect(tasks.filter((t) => batchRetryable(t, null, new Set())).map((t) => t.id)).toEqual(["j1"]);
  });

  it("实例正在移动：挂在它上面的失败（不分大小写）重试带原因挡下、批量不收；新建导入、别的实例与 Java 照旧；目录对不上的先说目录", () => {
    const moving = movingKeys([
      { id: "生存", activity: "moving" },
      { id: "Survival", activity: "moving" },
      { id: "创造", activity: "modifying" },
      { id: "空闲", activity: null },
    ]);
    expect([...moving]).toEqual(["生存", "survival"]);
    const failed = (id: string, overrides: Partial<Task>) => task(id, { status: "failed", error: { code: "io", message: "x" }, gameDir: A, ...overrides });
    const tasks = [
      backup(A, { id: "here" }),
      failed("latin", { kind: "mod", instanceId: "SURVIVAL", spec: { command: "install_mods", args: { versionId: "SURVIVAL", items: [] } } }),
      failed("create", { kind: "game", instanceId: "生存", spec: { command: "install_version", args: { targetName: "生存" } } }),
      failed("import", { kind: "modpack", instanceId: "生存", spec: { command: "install_modpack", args: { name: "生存" } } }),
      failed("other", { kind: "mod", instanceId: "创造", spec: { command: "install_mods", args: { versionId: "创造", items: [] } } }),
      task("j1", { status: "failed", gameDir: A }),
    ];
    expect(tasks.map((t) => retryBlockedReason(t, A, moving))).toEqual(["实例正在移到其他游戏目录", "实例正在移到其他游戏目录", null, null, null, null]);
    expect(tasks.filter((t) => batchRetryable(t, A, moving)).map((t) => t.id)).toEqual(["create", "import", "other", "j1"]);
    // 移动有了结局（不在 moving 里）就不挡
    expect(retryBlockedReason(tasks[0], A, new Set(["创造"]))).toBeNull();
    // 目录对不上是切回去之前一直成立的原因，先说它
    expect(retryBlockedReason(backup(B), A, moving)).toBe(`这个任务发起于游戏目录 ${B}，切回去再重试`);
  });

  it("移动成功后已结束的失败与取消跟着实例记到目标目录：同一实例（不分大小写）、已知发起于源目录的才迁；新建导入、移动、已完成、排队中、别的实例、别的目录与旧记录不动", () => {
    const C = "F:\\Other\\.minecraft";
    const done = { ...move(A), status: "done" as const, error: null, finishedAt: T0 };
    const mods = { kind: "mod" as const, spec: { command: "install_mods", args: { versionId: "生存", items: [] } } };
    const ended = (id: string, status: Task["status"], overrides: Partial<Task>) => task(id, { status, gameDir: A, finishedAt: T0, ...(status === "failed" ? { error: { code: "version_not_installed", message: "本地未安装版本 生存" } } : {}), ...overrides });
    const tasks = [
      done,
      ended("failed", "failed", { ...mods, instanceId: "生存" }),
      ended("cancelled", "cancelled", { ...mods, instanceId: "生存" }),
      ended("spelled", "failed", { ...mods, instanceId: "生存", gameDir: "d:/games/.minecraft/" }),
      ended("latin", "failed", { ...mods, instanceId: "survival" }),
      ended("create", "failed", { kind: "game", instanceId: "生存", spec: { command: "install_version", args: { targetName: "生存" } } }),
      ended("import", "cancelled", { kind: "modpack", instanceId: "生存", spec: { command: "install_modpack", args: { source: "D:/packs/生存.mrpack", name: "生存" } } }),
      ended("old-move", "failed", { kind: "instance_move", instanceId: "生存", spec: { command: "move_instance", args: { id: "生存", source: A, target: C } } }),
      ended("succeeded", "done", { ...mods, instanceId: "生存" }),
      task("queued", { ...mods, instanceId: "生存", status: "queued", gameDir: A }),
      ended("other", "failed", { ...mods, instanceId: "创造" }),
      ended("elsewhere", "failed", { ...mods, instanceId: "生存", gameDir: C }),
      ended("unknown", "failed", { ...mods, instanceId: "生存", gameDir: null }),
    ];
    const followed = followMove(tasks, done);
    expect(followed.filter((t) => t.gameDir === B).map((t) => t.id)).toEqual(["failed", "cancelled", "spelled"]);
    expect(followed.filter((t) => t.gameDir !== B).map((t) => [t.id, t.gameDir])).toEqual([
      ["m1", A],
      ["latin", A],
      ["create", A],
      ["import", A],
      ["old-move", A],
      ["succeeded", A],
      ["queued", A],
      ["other", A],
      ["elsewhere", C],
      ["unknown", null],
    ]);
    // 迁过去的只换游戏目录，结局与参数原样
    expect(followed[1]).toEqual({ ...tasks[1], gameDir: B });
    // 迁过去之后：源目录里不再挂着一个注定失败的重试，切到目标目录照常重试
    expect(retryGameDirReason(followed[1], A)).toBe(`这个任务发起于游戏目录 ${B}，切回去再重试`);
    expect(retryGameDirReason(followed[1], "e:/hmcl/.minecraft")).toBeNull();
    expect(commandGameDir(followed[1])).toBe(B);
    // 实例名不分大小写，与后端的锁键一致；旧日志里的移动没有 gameDir，按源目录认
    const latin = { ...done, gameDir: null, instanceId: "Survival", spec: { command: "move_instance", args: { id: "Survival", source: A, target: B } } };
    expect(followMove(tasks, latin).filter((t) => t.gameDir === B).map((t) => t.id)).toEqual(["latin"]);
    expect(() => followMove(tasks, tasks[1])).toThrow("任务 failed 不是移动实例");
    expect(() => followMove(tasks, { ...done, spec: { command: "move_instance", args: { id: "生存", source: A } } })).toThrow("移动任务 m1 的参数 target 应为字符串");
  });

  it("命令带的 gameDir：移动与 Java 下载不带，别的带发起时的目录；不知道发起于哪里的走到执行是状态层的错", () => {
    expect(commandGameDir(backup(A))).toBe(A);
    expect(commandGameDir(move(A))).toBeNull();
    expect(commandGameDir(task("j1", { gameDir: A }))).toBeNull();
    expect(() => commandGameDir(backup(null))).toThrow("任务 b1 不知道发起于哪个游戏目录，不能执行");
  });

  it("补全成功撤下被接替的失败新建：同一目录与不知道目录的撤，已知别的目录的留着", () => {
    const create = (id: string, gameDir: string | null) =>
      task(id, { kind: "game", instanceId: "生存", status: "failed", error: { code: "io", message: "x" }, spec: { command: "install_version", args: { targetName: "生存" } }, gameDir });
    const left = dropSuperseded([create("here", A), create("there", B), create("old", null)], "生存", "d:/games/.minecraft");
    expect(left.map((t) => t.id)).toEqual(["there"]);
    expect(inGameDir(create("there", B), null)).toBe(false);
    expect(inGameDir(create("old", null), null)).toBe(true);
  });

  it("移动成功时开不了工的任务：同一实例（不分大小写）、同一目录发起、还在排队的；新建导入、别的实例、别的目录、已开工或已结束的不算", () => {
    const running = { ...move(A), status: "running" as const, error: null };
    const queued = (id: string, overrides: Partial<Task>) => task(id, { status: "queued", gameDir: A, ...overrides });
    const mods = { kind: "mod" as const, spec: { command: "install_mods", args: { versionId: "生存", items: [] } } };
    const tasks = [
      running,
      queued("mods", { ...mods, instanceId: "生存" }),
      queued("pack", { kind: "resourcepack", instanceId: "生存", spec: { command: "install_resource", args: { versionId: "生存" } } }),
      queued("restore", { kind: "world_restore", instanceId: "生存", spec: { command: "restore_world_backup", args: { id: "生存", world: "w" } } }),
      queued("create", { kind: "game", instanceId: "生存", spec: { command: "install_version", args: { targetName: "生存" } } }),
      queued("other", { ...mods, instanceId: "创造" }),
      queued("elsewhere", { ...mods, instanceId: "生存", gameDir: B }),
      queued("java", {}),
      task("done", { ...mods, instanceId: "生存", status: "done", finishedAt: T0 }),
      task("failed", { ...mods, instanceId: "生存", status: "failed", error: { code: "io", message: "x" }, finishedAt: T0 }),
    ];
    expect(strandedByMove(tasks, running).map((t) => t.id)).toEqual(["mods", "pack", "restore"]);
    // 旧日志里的移动没有 gameDir，按源目录认
    expect(strandedByMove(tasks, { ...running, gameDir: null }).map((t) => t.id)).toEqual(["mods", "pack", "restore"]);
    // 实例名不分大小写，与后端的锁键一致
    const latin = { ...running, instanceId: "Survival", spec: { command: "move_instance", args: { id: "Survival", source: A, target: B } } };
    expect(strandedByMove([latin, queued("lower", { ...mods, instanceId: "survival" })], latin).map((t) => t.id)).toEqual(["lower"]);
    expect(() => strandedByMove(tasks, tasks[1])).toThrow("任务 mods 不是移动实例");
  });
});

describe("下载队列：状态容器", () => {
  let runs: Map<string, Deferred<unknown>>;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(T0);
    resetRecorder(ipc);
    bootOverrides(ipc);
    runs = new Map();
    ipc.overrides.set("install_java", (args) => {
      const d = deferred<unknown>();
      runs.set(String(args.taskId), d);
      return d.promise;
    });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const runtime = (raw: string) => ({ component: "java-runtime-delta", version: { major: 21, minor: 0, security: 5, build: 11, raw }, java_executable: "C:/java.exe" });

  it("四个任务：三个同时开始，第四个排队；一个完成后第四个立刻开始", async () => {
    const store = await startedStore();
    for (const major of [8, 17, 21, 25]) await store.actions.installJava(major);
    await settle();
    const tasks = store.getState().tasks;
    expect(tasks.map((t) => t.status)).toEqual(["running", "running", "running", "queued"]);
    expect(callsOf(ipc, "install_java").map((a) => a.requiredMajor)).toEqual([8, 17, 21]);
    expect(callsOf(ipc, "install_java").map((a) => a.taskId)).toEqual(tasks.slice(0, 3).map((t) => `${t.id}#1`));
    expect(queuePosition(tasks, tasks[3].id)).toBe(1);

    runs.get(`${tasks[1].id}#1`)?.resolve(runtime("17.0.12"));
    await settle();
    expect(store.getState().tasks.map((t) => t.status)).toEqual(["running", "done", "running", "running"]);
    expect(callsOf(ipc, "install_java").map((a) => a.requiredMajor)).toEqual([8, 17, 21, 25]);
    expect(store.getState().toasts.some((t) => t.title === "Java 17 已就绪")).toBe(true);
    store.stop();
  });

  it("全部重试：同一实例上的补全与装 Mod 一起重新排队时，只先开始补全，装 Mod 等补全结束再开始", async () => {
    const failed = (id: string, command: string, args: Record<string, unknown>, kind: Task["kind"]): TaskRecordDto =>
      record(id, "failed", { kind, instance_id: "生存", error: { code: "network_failed", message: "连不上" }, spec: { command, args } });
    ipc.overrides.set("load_task_journal", () => ({
      seq: 0,
      tasks: [
        failed("r1", "repair_instance", { id: "生存" }, "repair"),
        failed("m1", "install_mods", { versionId: "生存", platform: "modrinth", projectId: "sodium", items: [{ platform: "modrinth", projectId: "sodium", modVersionId: "v1", title: "Sodium", fileSize: 10 }] }, "mod"),
      ],
    }));
    const store = await startedStore();
    try {
      const repairDone = deferred<unknown>();
      ipc.overrides.set("repair_instance", () => repairDone.promise);
      ipc.overrides.set("install_mod", () => ({ file_name: "x.jar", path: "", platform: "modrinth", skipped: [] }));
      expect(store.actions.retryFailedTasks()).toBe(2);
      await settle();
      expect(callsOf(ipc, "repair_instance")).toHaveLength(1);
      expect(callsOf(ipc, "install_mod")).toHaveLength(0);
      repairDone.resolve(null);
      await settle();
      expect(callsOf(ipc, "install_mod")).toHaveLength(1);
    } finally {
      store.stop();
    }
  });

  it("取消排队中的任务只改状态，不发任何命令", async () => {
    const store = await startedStore();
    for (const major of [8, 17, 21, 25]) await store.actions.installJava(major);
    await settle();
    const queued = store.getState().tasks[3];
    const before = ipc.calls.length;
    await store.actions.cancelTask(queued.id);
    expect(ipc.calls.slice(before).map((c) => c.cmd)).toEqual([]);
    expect(store.getState().tasks[3].status).toBe("cancelled");
    expect(store.getState().toasts.at(-1)?.title).toContain("已取消");
    store.stop();
  });

  it("取消进行中的任务用这一次尝试的 taskId；命令以 cancelled 返回后落为已取消", async () => {
    const store = await startedStore();
    await store.actions.installJava(21);
    await settle();
    const t = store.getState().tasks[0];
    ipc.overrides.set("cancel_task", (args) => {
      runs.get(String(args.taskId))?.reject(cancelledError());
      return true;
    });
    await store.actions.cancelTask(t.id);
    await settle();
    expect(callsOf(ipc, "cancel_task")).toEqual([{ taskId: `${t.id}#1` }]);
    expect(store.getState().tasks[0].status).toBe("cancelled");
    store.stop();
  });

  it("失败后重试：排到队尾、换新的 taskId 重新执行", async () => {
    const store = await startedStore();
    await store.actions.installJava(21);
    await store.actions.installJava(17);
    await settle();
    const [first] = store.getState().tasks;
    runs.get(`${first.id}#1`)?.reject({ code: "network_failed", message: "连接超时" });
    await settle();
    expect(store.getState().tasks[0]).toMatchObject({ status: "failed", error: { code: "network_failed", message: "连接超时" } });
    expect(store.getState().toasts.at(-1)).toMatchObject({ tone: "error", action: { label: "重试" } });

    store.actions.retryTask(first.id);
    await settle();
    const after = store.getState().tasks;
    expect(after.map((t) => t.id)).toEqual([after[0].id, first.id]);
    expect(after[1]).toMatchObject({ status: "running", attempt: 2 });
    expect(callsOf(ipc, "install_java").map((a) => a.taskId)).toContain(`${first.id}#2`);
    store.stop();
  });

  it("下载 Java 时指定了实例：装好后把它的 Java 固定为新装的运行时，通知写明", async () => {
    const settings = { description: null, cover: null, cover_image: null, background: "follow", background_image: null, isolation: "follow_global", memory_mb: null, java: "auto", jvm_args: "", resolution: { width: 854, height: 480 }, fullscreen: false };
    ipc.overrides.set("get_instance_settings", () => settings);
    ipc.overrides.set("set_instance_settings", (args) => args.settings);
    const store = await startedStore();
    await store.actions.installJava(21, { pinFor: "生存" });
    await settle();
    const [t] = store.getState().tasks;
    runs.get(`${t.id}#1`)?.resolve(runtime("21.0.5"));
    await settle();
    const saved = callsOf(ipc, "set_instance_settings");
    expect(saved).toHaveLength(1);
    expect(saved[0]).toMatchObject({ id: "生存", settings: { java: "C:/java.exe", memory_mb: null } });
    expect(store.getState().toasts.filter((x) => x.tone === "success").map((x) => [x.title, x.body])).toEqual([["Java 21 已就绪", "「生存」下次启动会用它"]]);
    store.stop();
  });

  it("没指定实例的 Java 下载不改任何实例设置", async () => {
    const store = await startedStore();
    await store.actions.installJava(21);
    await settle();
    runs.get(`${store.getState().tasks[0].id}#1`)?.resolve(runtime("21.0.5"));
    await settle();
    expect(callsOf(ipc, "set_instance_settings")).toEqual([]);
    expect(store.getState().toasts.at(-1)).toMatchObject({ title: "Java 21 已就绪", body: "需要它的实例会自动使用" });
    store.stop();
  });

  it("同一件事重复入队只提示，不加任务", async () => {
    const store = await startedStore();
    await store.actions.installJava(21);
    await store.actions.installJava(21);
    expect(store.getState().tasks).toHaveLength(1);
    expect(store.getState().toasts.at(-1)?.title).toContain("Java 21");
    store.stop();
  });

  it("开始时刻记在开始运行的那一刻而不是入队时；用时与传输量取这一次尝试的开始时刻与累计", async () => {
    const store = await startedStore();
    for (const major of [8, 17, 21, 25]) await store.actions.installJava(major);
    await settle();
    const tasks = store.getState().tasks;
    expect(tasks.map((t) => t.startedAt)).toEqual([T0, T0, T0, null]);

    vi.setSystemTime(T0 + 4000);
    emit(CORE_EVENT, { kind: "download", task_id: `${tasks[0].id}#1`, total: 2, finished: 2, bytes: 700, total_bytes: 700, transferred: 300, speed: 1 });
    runs.get(`${tasks[0].id}#1`)?.resolve(runtime("8.0.412"));
    await settle();
    const after = store.getState().tasks;
    expect(after[0]).toMatchObject({ status: "done", startedAt: T0, finishedAt: T0 + 4000, doneBytes: 700, transferredBytes: 300 });
    // 第四个在通道空出的这一刻才开始
    expect(after[3]).toMatchObject({ status: "running", startedAt: T0 + 4000, doneBytes: 0, transferredBytes: 0 });
    store.stop();
  });

  it("任务日志存下开始时刻、累计字节与传输量；较早的日志没有这几项时，开始时刻与传输量为空、累计字节取最后一批", async () => {
    bootOverrides(ipc, {
      journal: [record("new", "done", { started_at: T0 - 9000, done_bytes: 52_000, transferred_bytes: 1200 }), record("zero", "done", { transferred_bytes: 0 }), record("old", "done")],
    });
    const store = await startedStore();
    expect(store.getState().tasks.map((t) => [t.id, t.startedAt, t.doneBytes, t.transferredBytes])).toEqual([
      ["new", T0 - 9000, 52_000, 1200],
      ["zero", null, 4096, 0],
      ["old", null, 4096, null],
    ]);
    await store.actions.flushTaskJournal();
    const saved = callsOf(ipc, "save_task_journal").at(-1)?.tasks as TaskRecordDto[];
    expect(saved.map((r) => [r.id, r.started_at, r.done_bytes, r.transferred_bytes])).toEqual([
      ["new", T0 - 9000, 52_000, 1200],
      ["zero", null, 4096, 0],
      ["old", null, 4096, null],
    ]);
    store.stop();
  });

  it("下载进度事件按这一次尝试的 taskId 对上任务", async () => {
    const store = await startedStore();
    await store.actions.installJava(21);
    await settle();
    const t = store.getState().tasks[0];
    emit(CORE_EVENT, { kind: "download", task_id: `${t.id}#1`, total: 4, finished: 1, bytes: 300, total_bytes: 1200, speed: 2048 });
    expect(store.getState().tasks[0]).toMatchObject({ bytes: 300, totalBytes: 1200, speedBps: 2048 });
    expect(store.getState().tasks[0].progress).toBeCloseTo(0.25, 10);
    store.stop();
  });

  it("步骤事件按这一次尝试的 taskId 进到那一步；表里有不认识的步骤直接报错，任务停在原来那一步", async () => {
    const store = await startedStore();
    await store.actions.installJava(21);
    await settle();
    const t = store.getState().tasks[0];
    emit(CORE_EVENT, { kind: "phase", task_id: `${t.id}#1`, plan: ["metadata", "libraries", "assets", "natives"], index: 2 });
    expect(store.getState().tasks[0].phase).toEqual({ plan: ["metadata", "libraries", "assets", "natives"], index: 2, batch: null, fraction: 0 });
    expect(store.getState().tasks[0].progress).toBeCloseTo(0.4, 10);
    expect(() => emit(CORE_EVENT, { kind: "phase", task_id: `${t.id}#1`, plan: ["metadata", "libraries", "assets", "bogus"], index: 3 })).toThrow("未知的安装步骤：bogus");
    expect(store.getState().tasks[0].phase).toMatchObject({ index: 2 });
    store.stop();
  });

  it("重启后：上次没完成的任务记为失败「关闭时中断」，写回任务日志，已结束的原样保留", async () => {
    bootOverrides(ipc, { journal: [record("r1", "running"), record("q1", "queued"), record("d1", "done")] });
    const store = await startedStore();
    const tasks = store.getState().tasks;
    expect(tasks.map((t) => [t.id, t.status])).toEqual([
      ["r1", "failed"],
      ["q1", "failed"],
      ["d1", "done"],
    ]);
    expect(tasks[0]).toMatchObject({ error: { code: TASK_INTERRUPTED_CODE, message: `${APP_NAME}关闭时中断` }, speedBps: 0, finishedAt: T0 });
    expect(callsOf(ipc, "install_java")).toEqual([]);
    await vi.advanceTimersByTimeAsync(1000);
    const saved = callsOf(ipc, "save_task_journal");
    expect(saved).toHaveLength(1);
    expect((saved[0].tasks as TaskRecordDto[]).map((r) => r.status)).toEqual(["failed", "failed", "done"]);
    store.stop();
  });
});
