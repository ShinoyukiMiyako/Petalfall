import { describe, expect, it } from "vitest";
import { instanceDto } from "../test/launcher";
import { instanceFromDto, type InstanceIncomplete } from "./adapt";
import { canRepair } from "./rules";

const marker = (kind: InstanceIncomplete["kind"]): InstanceIncomplete => ({ kind, outcome: "failed", startedAt: null, spec: null, error: null });
const LOST = "缺少版本 JSON，也没有记下当初装的是什么";

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
