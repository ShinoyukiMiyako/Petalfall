// @vitest-environment jsdom
// 几个账户一起看画的是不是默认皮肤（下载原版材质的入口据此出没）：与 useSkinInfo 共用缓存条目，说法同 skinSource。
import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SkinInfoDto } from "../../ipc/art";
import { IpcError } from "../../ipc/errors";
import { render, type Rendered } from "../../test/dom";
import type { IpcRecorder } from "../../test/fake-bridge";
import { callsOf, deferred, resetRecorder } from "../../test/launcher";
import { invalidate, resources } from "../resource";
import { useDrawsDefaultSkin, useSkinInfo } from "./home";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../../ipc/bridge", async (orig) => (await import("../../test/fake-bridge")).fakeBridge(await orig(), ipc));

const tick = () => act(async () => undefined);

const REMOTE: SkinInfoDto = { kind: "remote", sha1: "ab".repeat(20), model: "wide", hat: true };
const DEFAULT: SkinInfoDto = { kind: "default", path: "entity/player/wide/noor", model: "wide" };
const [ALEX, HANAMI, STEVE] = ["5d8e2c4b6a1f4e3d9c7b5a3f1e2d4c6b", "a3c9e1f2b4d64e8a9c0b1d2e3f4a5b6c", "0f3c5a7e9b1d4f6a8c2e4a6b8d0f2a4c"];

let seen: boolean[];
let view: Rendered | undefined;

function Probe({ uuids }: { uuids: readonly string[] }) {
  seen.push(useDrawsDefaultSkin(uuids));
  return null;
}

/** 每次都给一个新数组，同调用方 accounts.map(...) 的写法。 */
async function show(uuids: readonly string[]) {
  if (view === undefined) view = render(<Probe uuids={[...uuids]} />);
  else view.rerender(<Probe uuids={[...uuids]} />);
  await tick();
}

const last = () => {
  const value = seen.at(-1);
  if (value === undefined) throw new Error("Probe 没有渲染");
  return value;
};

beforeEach(() => {
  resetRecorder(ipc);
  seen = [];
  view?.unmount();
  view = undefined;
});

describe("useDrawsDefaultSkin", () => {
  it("远程皮肤与画人偶（null）都不算；加进一个画默认皮肤的账户就算，每个账户只请求一次", async () => {
    const skins: Record<string, SkinInfoDto | null> = { [ALEX]: REMOTE, [HANAMI]: null, [STEVE]: DEFAULT };
    ipc.overrides.set("skin_info", (args) => skins[args.accountUuid as string]);
    await show([ALEX, HANAMI]);
    expect(last()).toBe(false);
    await show([ALEX, HANAMI, STEVE]);
    expect(last()).toBe(true);
    expect(callsOf(ipc, "skin_info")).toEqual([{ accountUuid: ALEX }, { accountUuid: HANAMI }, { accountUuid: STEVE }]);
  });

  it("没有账户：不请求，不算", async () => {
    await show([]);
    expect(last()).toBe(false);
    expect(callsOf(ipc, "skin_info")).toEqual([]);
  });

  it("还在读时不算，读到默认皮肤才算", async () => {
    const skin = deferred<SkinInfoDto | null>();
    ipc.overrides.set("skin_info", () => skin.promise);
    await show([STEVE]);
    expect(last()).toBe(false);
    await act(async () => {
      skin.resolve(DEFAULT);
      await skin.promise;
    });
    expect(last()).toBe(true);
  });

  it("与 useSkinInfo 共用条目：头像已经读过的账户不再请求", async () => {
    ipc.overrides.set("skin_info", () => DEFAULT);
    function Both() {
      useSkinInfo(STEVE);
      seen.push(useDrawsDefaultSkin([STEVE]));
      return null;
    }
    view = render(<Both />);
    await tick();
    expect(last()).toBe(true);
    expect(callsOf(ipc, "skin_info")).toEqual([{ accountUuid: STEVE }]);
  });

  it("重取失败时按上一次读到的算：读到过默认皮肤的照旧算，从没读到过的不算", async () => {
    let fail = false;
    ipc.overrides.set("skin_info", (args) => {
      if (fail || args.accountUuid === ALEX) throw new IpcError("io", "皮肤信息读不出来");
      return DEFAULT;
    });
    await show([ALEX]);
    expect(last()).toBe(false);
    await show([STEVE]);
    expect(last()).toBe(true);
    fail = true;
    await act(async () => {
      invalidate("skin");
      await Promise.resolve();
    });
    await tick();
    expect(callsOf(ipc, "skin_info").filter((args) => args.accountUuid === STEVE)).toHaveLength(2);
    expect(last()).toBe(true);
  });

  it("调用方每次渲染给新数组、账户没变：不重新订阅，不重复请求；账户变了才订阅新的", async () => {
    ipc.overrides.set("skin_info", () => DEFAULT);
    const subscribe = vi.spyOn(resources, "subscribe");
    await show([STEVE]);
    await show([STEVE]);
    await show([STEVE]);
    expect(subscribe.mock.calls.map(([key]) => key)).toEqual([`skin:${STEVE}`]);
    expect(callsOf(ipc, "skin_info")).toEqual([{ accountUuid: STEVE }]);
    expect(last()).toBe(true);
    await show([STEVE, HANAMI]);
    expect(subscribe.mock.calls.map(([key]) => key)).toEqual([`skin:${STEVE}`, `skin:${STEVE}`, `skin:${HANAMI}`]);
  });
});
