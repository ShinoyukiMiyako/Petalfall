import { describe, expect, it } from "vitest";
import { installPhasesFromDto, taskFromRecord, taskToRecord } from "./adapt";
import { INSTALL_PHASES } from "./types";

describe("任务的适配：分步任务的步骤表", () => {
  it("认识的键原样进来，顺序不变；空表也照收", () => {
    expect(installPhasesFromDto(["metadata", "assets"])).toEqual(["metadata", "assets"]);
    expect(installPhasesFromDto([...INSTALL_PHASES].reverse())).toEqual([...INSTALL_PHASES].reverse());
    expect(installPhasesFromDto([])).toEqual([]);
  });

  it("表里有一个不认识的键就整张拒绝，不跳过它接着算第几步", () => {
    expect(() => installPhasesFromDto(["metadata", "bogus"])).toThrow("未知的安装步骤：bogus");
    expect(() => installPhasesFromDto(["Metadata"])).toThrow("未知的安装步骤：Metadata");
  });
});

describe("任务的适配：任务日志", () => {
  it("当前在哪一步不写进日志，读回的任务没有步骤", () => {
    const record = {
      id: "t1",
      kind: "game" as const,
      title: "安装 1.21.1",
      detail: "新实例「生存」",
      instance_id: "生存",
      status: "running" as const,
      progress: 0.4,
      bytes: 25_000_000,
      total_bytes: 50_000_000,
      done_bytes: 25_035_014,
      transferred_bytes: 25_035_014,
      speed_bps: 0,
      started_at: 1,
      attempt: 1,
      error: null,
      created_at: 0,
      finished_at: null,
      spec: { command: "install_version", args: { id: "1.21.1", targetName: "生存" } },
      game_dir: "D:\\Games\\.minecraft",
    };
    const task = { ...taskFromRecord(record), phase: { plan: ["metadata", "libraries"] as const, index: 1, batch: { total: 40, totalBytes: 50_000_000 } } };
    expect(taskToRecord(task)).toEqual(record);
    expect(taskFromRecord(record).phase).toBeNull();
  });

  it("发起时的游戏目录原样往返；较早的日志没有这一项读成 null（不拿别的目录补），写回仍是 null；类型不对按日志坏了报错", () => {
    const record = {
      id: "t1",
      kind: "world_backup" as const,
      title: "备份「新的世界」",
      detail: "「生存」",
      instance_id: "生存",
      status: "failed" as const,
      progress: 0,
      bytes: 0,
      total_bytes: null,
      speed_bps: 0,
      attempt: 1,
      error: { code: "io", message: "磁盘满了" },
      created_at: 0,
      finished_at: 1,
      spec: { command: "backup_world", args: { id: "生存", world: "新的世界" } },
    };
    expect(taskFromRecord({ ...record, game_dir: "E:\\HMCL\\.minecraft" }).gameDir).toBe("E:\\HMCL\\.minecraft");
    const old = taskFromRecord(record);
    expect(old.gameDir).toBeNull();
    expect(taskToRecord(old).game_dir).toBeNull();
    expect(taskFromRecord({ ...record, game_dir: null }).gameDir).toBeNull();
    expect(() => taskFromRecord({ ...record, game_dir: 42 as never })).toThrow("任务 t1 的 game_dir 应为字符串或 null");
  });
});
