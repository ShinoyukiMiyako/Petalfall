import { describe, expect, it } from "vitest";
import { instanceDto } from "../test/launcher";
import { instanceFromDto, type InstanceIncomplete } from "./adapt";
import { canRepair, repairRefusal } from "./rules";

const marker = (kind: InstanceIncomplete["kind"]): InstanceIncomplete => ({ kind, outcome: "failed", startedAt: null, spec: null, error: null });
const LOST = "缺少版本 JSON，也没有记下当初装的是什么";
/** 后端对「损坏且带整合包标记」的实例给的原因，与前端的重新导入说明措辞、标点都不一样。 */
const BACKEND_PACK = "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入";
const REIMPORT = "整合包没有导入完，缺的 Mod 与配置补全补不回来，删掉这个实例后重新导入。";

describe("补全修不好时给玩家的那一句", () => {
  it("整合包没导入完、后端也给了原因（版本 JSON 不可用时后端的真实组合）：说前端的重新导入说明，不照登后端那句", () => {
    expect(repairRefusal({ status: "broken", incomplete: marker("modpack"), unrepairableReason: BACKEND_PACK })).toBe(REIMPORT);
    // 版本 JSON 还在时后端不报原因：同一种情况说的是同一句
    expect(repairRefusal({ status: "broken", incomplete: marker("modpack"), unrepairableReason: null })).toBe(REIMPORT);
  });

  it("后端说补全修不好的：照登原文，句末有没有标点、后面跟不跟提示都不改", () => {
    const tail = ["", "。", ".", "！", "?"][Math.floor(Math.random() * 5)];
    const reason = `${LOST}${tail}`;
    expect(repairRefusal({ status: "broken", incomplete: null, unrepairableReason: reason })).toBe(reason);
    expect(repairRefusal({ status: "broken", incomplete: marker("unknown"), unrepairableReason: `${LOST}。目录里有一份叫 1.20.1.json 的版本文件` })).toBe(`${LOST}。目录里有一份叫 1.20.1.json 的版本文件`);
  });

  it("修得好的为 null；任意组合下有没有这一句与 canRepair 一一对应", () => {
    expect(repairRefusal({ status: "broken", incomplete: marker("install"), unrepairableReason: null })).toBeNull();
    expect(repairRefusal({ status: "ready", incomplete: null, unrepairableReason: null })).toBeNull();
    const kinds: (InstanceIncomplete["kind"] | null)[] = [null, "install", "modpack", "repair", "unknown"];
    for (let i = 0; i < 40; i++) {
      const kind = kinds[Math.floor(Math.random() * kinds.length)];
      const inst = {
        status: Math.random() < 0.8 ? ("broken" as const) : ("ready" as const),
        incomplete: kind === null ? null : marker(kind),
        unrepairableReason: Math.random() < 0.5 ? null : `原因${i}`,
      };
      expect(repairRefusal(inst) === null).toBe(canRepair(inst));
    }
  });
});

describe("点修复能不能修好", () => {
  it("后端说补全修不好的损坏实例修不好；后面跟着提示的原因同样算", () => {
    expect(canRepair({ status: "broken", incomplete: null, unrepairableReason: LOST })).toBe(false);
    expect(canRepair({ status: "broken", incomplete: marker("unknown"), unrepairableReason: `${LOST}。目录里有一份叫 1.20.1.json 的版本文件` })).toBe(false);
  });

  it("修得好的损坏实例、上次补全没做完的与就绪实例都修得好", () => {
    expect(canRepair({ status: "broken", incomplete: null, unrepairableReason: null })).toBe(true);
    expect(canRepair({ status: "broken", incomplete: marker("install"), unrepairableReason: null })).toBe(true);
    expect(canRepair({ status: "broken", incomplete: marker("repair"), unrepairableReason: null })).toBe(true);
    expect(canRepair({ status: "ready", incomplete: null, unrepairableReason: null })).toBe(true);
  });

  it("整合包没导入完的补全补不回来：版本 JSON 还在、后端没报原因时也修不好", () => {
    expect(canRepair({ status: "broken", incomplete: marker("modpack"), unrepairableReason: null })).toBe(false);
    expect(canRepair({ status: "broken", incomplete: marker("modpack"), unrepairableReason: "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入" })).toBe(false);
  });

  it("从列表项适配出来的实例直接能问：修不好的那个回答修不好，排上补全之后按安装中不再带原因", () => {
    const idle = { pins: [] as string[], busyTaskInstanceIds: new Set<string>() };
    const lost = instanceDto("旧电脑拷来的", { status: "broken", broken_reason: "缺少版本 JSON", unrepairable_reason: LOST, mc_version: null });
    expect(canRepair(instanceFromDto(lost, idle))).toBe(false);
    expect(canRepair(instanceFromDto(instanceDto("没装完", { status: "broken", broken_reason: "缺少版本 JSON", mc_version: null }), idle))).toBe(true);
    expect(instanceFromDto(lost, { pins: [], busyTaskInstanceIds: new Set(["旧电脑拷来的"]) })).toMatchObject({ status: "installing", unrepairableReason: null });
  });
});
