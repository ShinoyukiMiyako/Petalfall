// 损坏实例上导出、移动与进入存档被挡住的原因：点修复有没有用决定说法，修不好的不承诺「修复后才能……」。
import { describe, expect, it } from "vitest";
import { instanceFromDto, type InstanceSummary } from "../../domain/adapt";
import { locksOf } from "../../domain/instance-locks";
import type { InstanceDto } from "../../ipc/instances";
import { instanceDto } from "../../test/launcher";
import { brokenBlockReason, exportReason, moveReason } from "./ops";

const idle = { pins: [] as string[], busyTaskInstanceIds: new Set<string>() };
const LOST = "缺少版本 JSON，也没有记下当初装的是什么";
const REIMPORT = "整合包没有导入完，缺的 Mod 与配置补全补不回来，删掉这个实例后重新导入。";
const MODPACK_PENDING: InstanceDto["incomplete"] = { kind: "modpack", outcome: "pending", started_at: 1_700_000_000_000, spec: null, error: null };

const of = (over: Partial<InstanceDto>): InstanceSummary => instanceFromDto(instanceDto("生存", over), idle);
const lost = (reason: string) => of({ status: "broken", broken_reason: "缺少版本 JSON", unrepairable_reason: reason, mc_version: null });
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
    expect(reasons(lost(hinted)).enter).toBe(`实例文件不完整，补全修不好：${hinted}`);
    // 句末标点随机取一种（含没有）：有没有都照原文，不多也不少
    const tail = ["", "。", ".", "！", "?"][Math.floor(Math.random() * 5)];
    const reason = `Version JSON is missing ${Math.floor(Math.random() * 1000)}${tail}`;
    expect(reasons(lost(reason)).export).toBe(`实例文件不完整，补全修不好：${reason}`);
  });

  it("整合包没导入完的指向重新导入；后端同时报了原因也先说重新导入，与页面上给的入口一致", () => {
    expect(reasons(of({ incomplete: MODPACK_PENDING }))).toEqual({ enter: REIMPORT, export: REIMPORT, move: REIMPORT });
    const both = of({ status: "broken", broken_reason: "整合包没有导入完", unrepairable_reason: LOST, incomplete: MODPACK_PENDING });
    expect(reasons(both)).toEqual({ enter: REIMPORT, export: REIMPORT, move: REIMPORT });
  });

  it("没坏的实例导出与移动照常可用", () => {
    const ready = of({});
    expect(exportReason(ready, null)).toBeUndefined();
    expect(moveReason(ready, locksOf(ready, false))).toBeUndefined();
  });
});
