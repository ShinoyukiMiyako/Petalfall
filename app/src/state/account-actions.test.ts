// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ACCOUNT_CHANGED_EVENT, type AccountDto } from "../ipc/accounts";
import { IpcError } from "../ipc/errors";
import { emitMockEvent } from "../ipc/mock/events";
import type { IpcRecorder } from "../test/fake-bridge";
import { accountDto, bootOverrides, callsOf, resetRecorder, settle, startedStore } from "../test/launcher";
import { resources } from "./resource";
import type { LauncherStore } from "./store";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../ipc/bridge", async (orig) => (await import("../test/fake-bridge")).fakeBridge(await orig(), ipc));

const MS = accountDto({ uuid: "ms-1", name: "Mock_Player", account_type: "microsoft", last_verified_at: 1 });
const SKIN = accountDto({ uuid: "ys-1", name: "皮肤玩家", account_type: "authlib_injector", auth_server: "皮肤站", server_url: "https://skin.example.com/api/yggdrasil" });

let accounts: AccountDto[];
let store: LauncherStore;

/** 缓存一份皮肤信息，返回它被请求的次数（读两次，确认已缓存住）。 */
async function cacheSkin(uuid: string): Promise<() => Promise<number>> {
  const fetcher = vi.fn(() => Promise.resolve({ uuid }));
  await resources.read(`skin:${uuid}`, fetcher);
  await resources.read(`skin:${uuid}`, fetcher);
  expect(fetcher).toHaveBeenCalledTimes(1);
  return async () => {
    await resources.read(`skin:${uuid}`, fetcher);
    return fetcher.mock.calls.length;
  };
}

beforeEach(async () => {
  vi.useFakeTimers();
  vi.setSystemTime(1_720_000_000_000);
  resetRecorder(ipc);
  accounts = [MS, SKIN];
  bootOverrides(ipc, { accounts });
  ipc.overrides.set("list_accounts", () => accounts);
  store = await startedStore();
});
afterEach(() => {
  store.stop();
  vi.useRealTimers();
});

describe("刷新账户", () => {
  it("凭据失效：账户重新读出来转为 expired，返回 expired，不抛错也不发通知", async () => {
    ipc.overrides.set("refresh_account", () => {
      accounts = [{ ...MS, status: "expired" }, SKIN];
      throw new IpcError("ReloginRequired", "登录凭据已失效，需要重新登录");
    });
    const skin = await cacheSkin("ms-1");
    expect(await store.actions.refreshAccount("ms-1")).toBe("expired");
    expect(store.getState().accounts.find((a) => a.uuid === "ms-1")?.status).toBe("expired");
    expect(store.getState().toasts).toEqual([]);
    expect(callsOf(ipc, "list_accounts")).toHaveLength(2);
    // 失效时皮肤没有重新拉取，缓存不必作废
    expect(await skin()).toBe(1);
  });

  it("成功：返回 refreshed，账户重读，皮肤缓存作废", async () => {
    ipc.overrides.set("refresh_account", () => MS);
    const skin = await cacheSkin("ms-1");
    const other = await cacheSkin("ys-1");
    expect(await store.actions.refreshAccount("ms-1")).toBe("refreshed");
    expect(await skin()).toBe(2);
    expect(await other()).toBe(1);
  });

  it("应用未获批准：通知说清是 client_id 的事并给去设置的入口，不当成账户失效，也不算失败", async () => {
    ipc.overrides.set("refresh_account", () => {
      throw new IpcError("AppNotApproved", "Invalid app registration");
    });
    expect(await store.actions.refreshAccount("ms-1")).toBe("not_approved");
    expect(store.getState().accounts.find((a) => a.uuid === "ms-1")?.status).not.toBe("expired");
    expect(store.getState().toasts).toMatchObject([{ tone: "error", title: "登录应用未获批准", action: { label: "打开设置", to: { page: "settings", section: "general" } } }]);
  });

  it("这个版本没有 client_id：通知说清缺的是什么并给去设置的入口，账户不当成失效", async () => {
    ipc.overrides.set("refresh_account", () => {
      throw new IpcError("MsaClientIdMissing", "这个版本没有内置微软登录 client_id");
    });
    expect(await store.actions.refreshAccount("ms-1")).toBe("no_client_id");
    expect(store.getState().accounts.find((a) => a.uuid === "ms-1")?.status).not.toBe("expired");
    expect(store.getState().toasts).toMatchObject([{ tone: "error", title: "这个版本没有内置微软登录", action: { label: "打开设置", to: { page: "settings", section: "general" } } }]);
  });

  it("其余失败原样抛出", async () => {
    ipc.overrides.set("refresh_account", () => {
      throw new IpcError("Http", "网络连接失败");
    });
    await expect(store.actions.refreshAccount("ms-1")).rejects.toMatchObject({ code: "Http" });
  });
});

describe("登录成功后皮肤缓存作废", () => {
  it("外置登录（含重新登录）", async () => {
    ipc.overrides.set("authlib_login", () => ({ status: "logged_in", account: SKIN }));
    const skin = await cacheSkin("ys-1");
    await store.actions.authlibLogin("https://skin.example.com/api/yggdrasil", "a@example.com", "pw");
    expect(await skin()).toBe(2);
  });

  it("外置登录选角色", async () => {
    ipc.overrides.set("select_authlib_profile", () => SKIN);
    const skin = await cacheSkin("ys-1");
    await store.actions.selectAuthlibProfile("pending-1", "ys-1");
    expect(await skin()).toBe(2);
  });

  it("微软登录（同一 UUID 的账户重新登录）", async () => {
    ipc.overrides.set("microsoft_login", () => MS);
    const skin = await cacheSkin("ms-1");
    await store.actions.startMicrosoftLogin();
    expect(await skin()).toBe(2);
  });
});

describe("后端在后台改写了账户记录", () => {
  it("续期换了名字与皮肤：账户重读、名字换上，有人在看的皮肤立即重取，别的账户的皮肤不动", async () => {
    const fetchSkin = vi.fn(() => Promise.resolve({ kind: "remote", sha1: "ab".repeat(20), model: "wide", hat: true }));
    const unsubscribe = resources.subscribe("skin:ms-1", fetchSkin, () => undefined);
    await settle();
    expect(fetchSkin).toHaveBeenCalledTimes(1);
    const other = await cacheSkin("ys-1");
    const listed = callsOf(ipc, "list_accounts").length;

    accounts = [{ ...MS, name: "Renamed_Player", last_verified_at: 2 }, SKIN];
    emitMockEvent(ACCOUNT_CHANGED_EVENT, { uuid: "ms-1" });
    await settle();
    expect(store.getState().accounts.find((a) => a.uuid === "ms-1")).toMatchObject({ name: "Renamed_Player", lastVerifiedAt: 2 });
    expect(callsOf(ipc, "list_accounts")).toHaveLength(listed + 1);
    expect(fetchSkin).toHaveBeenCalledTimes(2);
    expect(await other()).toBe(1);
    unsubscribe();
  });

  it("通知积压、不知道是哪几个账户（uuid 为 null）：整份重读账户列表，每个账户的皮肤都重取", async () => {
    const ms = await cacheSkin("ms-1");
    const ys = await cacheSkin("ys-1");
    const listed = callsOf(ipc, "list_accounts").length;

    accounts = [{ ...MS, name: "Renamed_Player" }, { ...SKIN, name: "改过的角色" }];
    emitMockEvent(ACCOUNT_CHANGED_EVENT, { uuid: null });
    await settle();
    expect(callsOf(ipc, "list_accounts")).toHaveLength(listed + 1);
    expect(store.getState().accounts.map((a) => a.name)).toEqual(["Renamed_Player", "改过的角色"]);
    expect(await ms()).toBe(2);
    expect(await ys()).toBe(2);
  });

  it("没人在看的皮肤缓存作废，下次要时重取；重读账户失败发通知，不往外抛", async () => {
    const skin = await cacheSkin("ys-1");
    ipc.overrides.set("list_accounts", () => {
      throw new IpcError("Dpapi", "凭据库解不开");
    });
    emitMockEvent(ACCOUNT_CHANGED_EVENT, { uuid: "ys-1" });
    await settle();
    expect(await skin()).toBe(2);
    expect(store.getState().toasts).toMatchObject([{ tone: "error", title: "账户列表没能刷新" }]);
  });
});

describe("打开外部网址", () => {
  it("http(s) 网址交给 opener 插件的命令；其余协议直接拒绝，不发命令；打开失败原样抛出", async () => {
    ipc.overrides.set("plugin:opener|open_url", () => null);
    await store.actions.openExternal("https://www.microsoft.com/link");
    expect(callsOf(ipc, "plugin:opener|open_url")).toEqual([{ url: "https://www.microsoft.com/link" }]);

    await expect(store.actions.openExternal("file:///C:/Windows/System32/calc.exe")).rejects.toThrow("只能打开 http(s) 网址");
    expect(callsOf(ipc, "plugin:opener|open_url")).toHaveLength(1);

    ipc.overrides.set("plugin:opener|open_url", () => {
      throw "url not allowed on the configured scope: https://evil.example";
    });
    await expect(store.actions.openExternal("https://evil.example")).rejects.toMatchObject({ code: "unknown" });
  });
});
