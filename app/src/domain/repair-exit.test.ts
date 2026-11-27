import { describe, expect, it } from "vitest";
import { REPAIR_EXIT_COPY, sentence } from "../copy/flows";
import { instanceDto } from "../test/launcher";
import { instanceFromDto, type InstanceIncomplete, type InstanceSummary } from "./adapt";
import { canRepair, needsReimport, repairExit, repairRefusal, type RepairExit } from "./rules";

type Inst = Pick<InstanceSummary, "status" | "incomplete" | "unrepairableReason" | "unrepairableKind" | "renameTo">;

const marker = (kind: InstanceIncomplete["kind"]): InstanceIncomplete => ({ kind, outcome: "failed", startedAt: null, spec: null, error: null });
const UNREADABLE = "版本 JSON 读不出来：文件 IO 失败: D:\\Games\\.minecraft\\versions\\锁着的\\锁着的.json：另一个程序正在使用此文件，进程无法访问。 (os error 32)";
const LOST = "缺少版本 JSON，也没有记下当初装的是什么";
const HINTED = `${LOST}；目录里有一份叫 1.20.1-Fabric.json 的版本文件，把实例改名成 1.20.1-Fabric 就能认出来`;
const PACK = "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入";

const inst = (over: Partial<Inst>): Inst => ({ status: "broken", incomplete: null, unrepairableReason: null, unrepairableKind: null, renameTo: null, ...over });

describe("补全修不好时剩下的出路", () => {
  it.each<[string, Inst, RepairExit | null]>([
    ["就绪的实例没坏", inst({ status: "ready" }), null],
    ["修得好的损坏实例：没有原因", inst({}), null],
    ["新建没装完、记着当初装的是什么", inst({ incomplete: marker("install") }), null],
    ["上次补全没做完", inst({ incomplete: marker("repair") }), null],
    ["安装中的整合包半成品还没下结论", inst({ status: "installing", incomplete: marker("modpack") }), null],
    ["整合包没导入完、版本 JSON 还在（后端不报原因）", inst({ incomplete: marker("modpack") }), { kind: "reimport" }],
    ["整合包没导入完、版本 JSON 也没了", inst({ incomplete: marker("modpack"), unrepairableReason: PACK, unrepairableKind: "modpack" }), { kind: "reimport" }],
    ["整合包标记压过读不出来：缺的 Mod 只有重新导入补得回来", inst({ incomplete: marker("modpack"), unrepairableReason: UNREADABLE, unrepairableKind: "unreadable" }), { kind: "reimport" }],
    ["版本 JSON 读不出来", inst({ unrepairableReason: UNREADABLE, unrepairableKind: "unreadable" }), { kind: "rescan" }],
    ["版本 JSON 读不出来，带着安装标记也一样：文件可能完好，不引去删除", inst({ incomplete: marker("install"), unrepairableReason: UNREADABLE, unrepairableKind: "unreadable" }), { kind: "rescan" }],
    ["读不出来压过改名目标", inst({ unrepairableReason: UNREADABLE, unrepairableKind: "unreadable", renameTo: "1.20.1-Fabric" }), { kind: "rescan" }],
    ["没有记录、目录里有一份改个名就认得出的版本 JSON", inst({ unrepairableReason: HINTED, unrepairableKind: "no_record", renameTo: "1.20.1-Fabric" }), { kind: "rename", to: "1.20.1-Fabric" }],
    ["没有记录也没有可改的名字", inst({ unrepairableReason: LOST, unrepairableKind: "no_record" }), { kind: "delete" }],
    ["标记读不出来、也没有记录", inst({ incomplete: marker("unknown"), unrepairableReason: LOST, unrepairableKind: "no_record" }), { kind: "delete" }],
    ["只给了原因没给种类", inst({ unrepairableReason: LOST }), { kind: "delete" }],
  ])("%s", (_, input, exit) => {
    expect(repairExit(input)).toEqual(exit);
  });

  it("任意组合下：没有出路当且仅当 repairRefusal 为 null（也就是修得好）；重新导入当且仅当 needsReimport；改名的目标就是 renameTo", () => {
    const statuses: Inst["status"][] = ["ready", "installing", "broken"];
    const markers: (InstanceIncomplete["kind"] | null)[] = [null, "install", "repair", "modpack", "unknown"];
    const reasons = [null, UNREADABLE, LOST, HINTED, PACK];
    const kinds: Inst["unrepairableKind"][] = [null, "unreadable", "no_record", "modpack"];
    const names = [null, "1.20.1-Fabric", `改${Math.floor(Math.random() * 1000)}`];
    let checked = 0;
    for (const status of statuses)
      for (const kind of markers)
        for (const unrepairableReason of reasons)
          for (const unrepairableKind of kinds)
            for (const renameTo of names) {
              const input: Inst = { status, incomplete: kind === null ? null : marker(kind), unrepairableReason, unrepairableKind, renameTo };
              const exit = repairExit(input);
              const label = JSON.stringify(input);
              expect(exit === null, label).toBe(repairRefusal(input) === null);
              expect(exit === null, label).toBe(canRepair(input));
              expect(exit !== null && exit.kind === "reimport", label).toBe(needsReimport(input));
              if (exit !== null && exit.kind === "rename") expect(exit.to, label).toBe(renameTo);
              if (exit !== null && exit.kind === "rescan") expect(unrepairableKind, label).toBe("unreadable");
              checked += 1;
            }
    expect(checked).toBe(3 * 5 * 5 * 4 * 3);
  });

  it("从列表项适配出来的实例直接能问：读不出来的给重新扫描，带改名目标的给改名，排上补全之后没有出路", () => {
    const idle = { pins: [] as string[], busyTaskInstanceIds: new Set<string>() };
    const locked = instanceDto("锁着的", { status: "broken", broken_reason: UNREADABLE, unrepairable_reason: UNREADABLE, unrepairable_kind: "unreadable", mc_version: null });
    expect(repairExit(instanceFromDto(locked, idle))).toEqual({ kind: "rescan" });
    const moved = instanceDto("改过名的", { status: "broken", broken_reason: "缺少版本 JSON", unrepairable_reason: HINTED, unrepairable_kind: "no_record", rename_to: "1.20.1-Fabric", mc_version: null });
    expect(repairExit(instanceFromDto(moved, idle))).toEqual({ kind: "rename", to: "1.20.1-Fabric" });
    expect(repairExit(instanceFromDto(moved, { pins: [], busyTaskInstanceIds: new Set(["改过名的"]) }))).toBeNull();
  });
});

describe("出路的文案", () => {
  it("重新扫描与改名按钮、读不出来时接在原因后面的那句", () => {
    expect(REPAIR_EXIT_COPY.rescan).toBe("重新扫描");
    expect(REPAIR_EXIT_COPY.rename("1.20.1-Fabric")).toBe("改名为「1.20.1-Fabric」");
    expect(`${sentence(UNREADABLE)}${REPAIR_EXIT_COPY.unreadableHint}`).toBe(
      "版本 JSON 读不出来：文件 IO 失败: D:\\Games\\.minecraft\\versions\\锁着的\\锁着的.json：另一个程序正在使用此文件，进程无法访问。 (os error 32)。关掉占用它的程序或修好读取权限后，重新扫描就能恢复。",
    );
  });
});
