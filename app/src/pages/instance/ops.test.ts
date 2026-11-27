// 损坏实例上导出、移动与进入存档被挡住的原因：点修复有没有用决定说法，修不好的不承诺「修复后才能……」。
import { describe, expect, it } from "vitest";
import { instanceFromDto, type InstanceSummary } from "../../domain/adapt";
import { locksOf } from "../../domain/instance-locks";
import type { InstanceDto } from "../../ipc/instances";
import { instanceDto } from "../../test/launcher";
import { brokenBlockReason, exportReason, moveReason, repairReason } from "./ops";

const idle = { pins: [] as string[], busyTaskInstanceIds: new Set<string>() };
const LOST = "缺少版本 JSON，也没有记下当初装的是什么";
const REIMPORT = "整合包没有导入完，缺的 Mod 与配置补全补不回来，删掉这个实例后重新导入。";
const MODPACK_PENDING: InstanceDto["incomplete"] = { kind: "modpack", outcome: "pending", started_at: 1_700_000_000_000, spec: null, error: null };
const INSTALL_PENDING: InstanceDto["incomplete"] = {
  kind: "install",
  outcome: "pending",
  started_at: 1_700_000_000_000,
  spec: { mc_version: "1.21.1", loader: "fabric", loader_version: "0.16.10" },
  error: null,
};
const UNREADABLE = "版本 JSON 读不出来：文件 IO 失败: D:\\Games\\.minecraft\\versions\\生存\\生存.json：另一个程序正在使用此文件，进程无法访问。 (os error 32)";
const RENAMABLE = `${LOST}；目录里有一份叫 1.20.1-NeoForge.json 的版本文件，把实例改名成 1.20.1-NeoForge 就能认出来`;

const of = (over: Partial<InstanceDto>): InstanceSummary => instanceFromDto(instanceDto("生存", over), idle);
const lost = (reason: string, renameTo: string | null = null) =>
  of({ status: "broken", broken_reason: "缺少版本 JSON", unrepairable_reason: reason, unrepairable_kind: "no_record", rename_to: renameTo, mc_version: null });
// 读不出来的带着记下安装内容的安装标记：有记录也修不好
const unreadable = () =>
  of({ status: "broken", broken_reason: UNREADABLE, unrepairable_reason: UNREADABLE, unrepairable_kind: "unreadable", incomplete: INSTALL_PENDING, mc_version: null });
const renamable = () =>
  of({ status: "broken", broken_reason: "缺少版本 JSON", unrepairable_reason: RENAMABLE, unrepairable_kind: "no_record", rename_to: "1.20.1-NeoForge", mc_version: null });
const reasons = (inst: InstanceSummary) => ({
  enter: brokenBlockReason(inst, "实例需要先修复"),
  export: exportReason(inst, null),
  move: moveReason(inst, locksOf(inst, false)),
});

describe("损坏实例被挡住的原因", () => {
  it("补全修得好的照旧说修复后才能做", () => {
    expect(reasons(of({ status: "broken", broken_reason: "缺少客户端 jar" }))).toEqual({
      enter: "实例需要先修复",
      export: "实例文件不完整，修复后才能导出",
      move: "实例文件不完整，修复后才能移动",
    });
  });

  it("后端说补全修不好的不承诺修复，写明后端给的原因", () => {
    const line = `实例文件不完整，补全修不好：${LOST}`;
    expect(reasons(lost(LOST))).toEqual({ enter: line, export: line, move: line });
  });

  it("后端原因已带句末标点或后面跟着一句提示：原样接在末尾，不改写也不补句号", () => {
    const hinted = `${LOST}；目录里有一份叫 1.20.1.json 的版本文件，把实例改名成 1.20.1 就能认出来`;
    expect(reasons(lost(hinted, "1.20.1")).enter).toBe(`实例文件不完整，补全修不好：${hinted}`);
    // 句末标点随机取一种（含没有）：有没有都照原文，不多也不少
    const tail = ["", "。", ".", "！", "?"][Math.floor(Math.random() * 5)];
    const reason = `Version JSON is missing ${Math.floor(Math.random() * 1000)}${tail}`;
    expect(reasons(lost(reason)).export).toBe(`实例文件不完整，补全修不好：${reason}`);
  });

  it("版本 JSON 读不出来的不说文件不完整：直接用后端原因，不冠「补全修不好」的前缀", () => {
    expect(reasons(unreadable())).toEqual({ enter: UNREADABLE, export: UNREADABLE, move: UNREADABLE });
  });

  it("带改名目标的照旧冠上前缀，后面是后端原因连同改名提示", () => {
    const line = `实例文件不完整，补全修不好：${RENAMABLE}`;
    expect(reasons(renamable())).toEqual({ enter: line, export: line, move: line });
  });

  it("整合包没导入完的指向重新导入；后端同时报了原因也先说重新导入，与页面上给的入口一致", () => {
    expect(reasons(of({ incomplete: MODPACK_PENDING }))).toEqual({ enter: REIMPORT, export: REIMPORT, move: REIMPORT });
    const both = of({ status: "broken", broken_reason: "整合包没有导入完", unrepairable_reason: LOST, unrepairable_kind: "no_record", incomplete: MODPACK_PENDING });
    expect(reasons(both)).toEqual({ enter: REIMPORT, export: REIMPORT, move: REIMPORT });
  });

  it("补全按钮的原因：整合包没导入完、后端也给了它的那句原因时，同样先说前端的重新导入说明", () => {
    const backendPack = "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入";
    const pack = of({ status: "broken", broken_reason: "缺少版本 JSON", unrepairable_reason: backendPack, unrepairable_kind: "modpack", incomplete: MODPACK_PENDING, mc_version: null });
    expect(repairReason(pack, locksOf(pack, false))).toBe(REIMPORT);
    // 版本 JSON 还在、后端没报原因的同一种情况：说的是同一句
    const intact = of({ incomplete: MODPACK_PENDING });
    expect(repairReason(intact, locksOf(intact, false))).toBe(REIMPORT);
    expect(repairReason(lost(LOST), locksOf(lost(LOST), false))).toBe(LOST);
    const repairable = of({ status: "broken", broken_reason: "缺少客户端 jar" });
    expect(repairReason(repairable, locksOf(repairable, false))).toBeUndefined();
  });

  it("同一实例的补全原因与被挡住的原因取自同一判定：修不好的那句一字不差，只按出路决定冠不冠前缀", () => {
    const backendPack = "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入";
    const cases: [string, InstanceSummary, { repair: string | undefined; blocked: string }][] = [
      ["修得好", of({ status: "broken", broken_reason: "缺少客户端 jar" }), { repair: undefined, blocked: "实例需要先修复" }],
      // 后端那句与前端的重新导入说明措辞不同：两边都得是前端那句，任何一边自己另判先后都会在这里分叉
      [
        "整合包没导入完、后端也报了原因",
        of({ status: "broken", broken_reason: "缺少版本 JSON", unrepairable_reason: backendPack, unrepairable_kind: "modpack", incomplete: MODPACK_PENDING, mc_version: null }),
        { repair: REIMPORT, blocked: REIMPORT },
      ],
      ["读不出来", unreadable(), { repair: UNREADABLE, blocked: UNREADABLE }],
      ["带改名目标", renamable(), { repair: RENAMABLE, blocked: `实例文件不完整，补全修不好：${RENAMABLE}` }],
      ["没有记录", lost(LOST), { repair: LOST, blocked: `实例文件不完整，补全修不好：${LOST}` }],
    ];
    for (const [name, inst, expected] of cases) {
      expect({ name, repair: repairReason(inst, locksOf(inst, false)), blocked: brokenBlockReason(inst, "实例需要先修复") }).toEqual({ name, ...expected });
    }
  });

  it("没坏的实例导出与移动照常可用", () => {
    const ready = of({});
    expect(exportReason(ready, null)).toBeUndefined();
    expect(moveReason(ready, locksOf(ready, false))).toBeUndefined();
  });
});
