// @vitest-environment jsdom
// 设置补丁从状态层经 update_config 落到假后端、再由重读的配置回到 settings：新开关不能在哪一层被丢掉或改名。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { resetMocks } from "../ipc/mock";
import type { IpcRecorder } from "../test/fake-bridge";
import { bootOverrides, callsOf, resetRecorder, startedStore } from "../test/launcher";
import type { LauncherStore } from "./store";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../ipc/bridge", async (orig) => (await import("../test/fake-bridge")).fakeBridge(await orig(), ipc));

let store: LauncherStore;

beforeEach(async () => {
  vi.useFakeTimers();
  resetMocks();
  resetRecorder(ipc);
  bootOverrides(ipc);
  // 配置的读写都落到假后端，读回来的才是写进去的
  ipc.overrides.delete("get_config");
  ipc.overrides.delete("update_config");
  store = await startedStore();
});
afterEach(() => {
  store.stop();
  vi.useRealTimers();
});

describe("自动下载受限的 CurseForge 文件", () => {
  it("首查读到默认关；打开写出 downloadRestrictedCurseforgeFiles: true，settings 跟着变，相邻的自动下载 Java 不动", async () => {
    expect(store.getState().settings).toMatchObject({ downloadRestrictedCurseforgeFiles: false, autoDownloadJava: true });

    await store.actions.updateSettings({ downloadRestrictedCurseforgeFiles: true });
    expect(callsOf(ipc, "update_config")).toEqual([{ downloadRestrictedCurseforgeFiles: true }]);
    expect(store.getState().settings).toMatchObject({ downloadRestrictedCurseforgeFiles: true, autoDownloadJava: true });

    await store.actions.updateSettings({ downloadRestrictedCurseforgeFiles: false });
    expect(callsOf(ipc, "update_config")).toEqual([{ downloadRestrictedCurseforgeFiles: true }, { downloadRestrictedCurseforgeFiles: false }]);
    expect(store.getState().settings?.downloadRestrictedCurseforgeFiles).toBe(false);
  });
});
