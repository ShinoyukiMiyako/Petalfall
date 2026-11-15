// @vitest-environment jsdom
// @module-tag heavy-page
// 整合包开始下载后进度块下面那句说明：新实例有没有出现在侧栏，按入队那一刻侧栏还有没有空位定。
// 导入一直不返回，面板停在进度态。
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubCanvas } from "../../../art/illustrations/test-canvas";
import { SIDEBAR_PIN_LIMIT } from "../../../domain/limits";
import { OverlayRoot } from "../../../shell/overlay";
import { ShellProvider } from "../../../shell/shell";
import { LauncherProvider, useLauncher, type LauncherApi } from "../../../state/launcher";
import type { ModVersionList, ProjectDetail } from "../../../state/queries/discover";
import type { ResourceResult } from "../../../state/resource";
import { drainFrames, render, stubMatchMedia, stubResizeObserver, type Rendered } from "../../../test/dom";
import type { IpcArgs, IpcRecorder } from "../../../test/fake-bridge";
import { bootOverrides, callsOf, instanceDto, resetRecorder, settle } from "../../../test/launcher";
import { MODPACK_COPY } from "../copy";
import { modVersion } from "../fixtures";
import { ModpackPanel } from "./ModpackPanel";

const ipc = vi.hoisted((): IpcRecorder => ({ calls: [], overrides: new Map() }));
vi.mock("../../../ipc/bridge", async (orig) => (await import("../../../test/fake-bridge")).fakeBridge(await orig(), ipc));

const PACK = "樱落";
const VERSION = modVersion("mp-1", { project_id: "MP1", version_number: "2.0.0", loaders: ["fabric"], game_versions: ["1.21.1"] });
const IN_SIDEBAR = "已出现在侧栏与实例库里，关掉这个窗口不影响下载。";
const LIBRARY_ONLY = "已出现在实例库里，关掉这个窗口不影响下载。";
const EXISTING = ["生存", "创造", "空岛", "机械", "魔法"];

const DETAIL: ProjectDetail = {
  platform: "modrinth",
  project_id: "MP1",
  slug: "sakura",
  title: PACK,
  summary: "整合包",
  body: "",
  body_format: "markdown",
  author: "作者",
  downloads: 100,
  follows: 1,
  icon_url: null,
  gallery: [],
  hue: 330,
  categories: [],
  resource_type: "modpack",
  loaders: ["fabric"],
  game_versions: ["1.21.1"],
  date_published: null,
  date_modified: null,
  page_url: null,
  source_url: null,
  issues_url: null,
  wiki_url: null,
  latest_version: VERSION,
  dependencies: [],
  distribution_allowed: true,
};

const VERSIONS: ResourceResult<ModVersionList> = {
  status: "ready",
  data: { versions: [VERSION], truncated: false },
  error: null,
  refreshing: false,
  reload: () => undefined,
  refresh: () => undefined,
};

let view: Rendered | undefined;
let footer: HTMLDivElement;
let launcher: LauncherApi | undefined;

const flush = () =>
  act(async () => {
    await settle();
  });

const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });

function Probe() {
  launcher = useLauncher();
  return null;
}

function api(): LauncherApi {
  if (launcher === undefined) throw new Error("还没有挂载");
  return launcher;
}

async function mountAndInstall(pins: readonly string[]) {
  bootOverrides(ipc, { instances: EXISTING.map((id) => instanceDto(id)), pins: [...pins] });
  view = render(
    <LauncherProvider>
      <ShellProvider>
        <ModpackPanel detail={DETAIL} origins={[]} versions={VERSIONS} pinned={null} onPin={() => undefined} online showingPrevious={false} footerHost={footer} onClose={() => undefined} />
        <Probe />
        <OverlayRoot />
      </ShellProvider>
    </LauncherProvider>,
  );
  await flush();
  const install = [...footer.querySelectorAll("button")].find((b) => b.textContent === MODPACK_COPY.install);
  if (install === undefined) throw new Error("底栏没有安装按钮");
  act(() => install.click());
  await flush();
  expect(callsOf(ipc, "install_modpack").map((a) => a.name)).toEqual([PACK]);
  // 进度块画出第一帧才开始淡入，推一段让它提交
  await advance(100);
}

/** 面板上两种说法里出现的那几条。 */
function backgroundNotes(): (string | null)[] {
  return [...document.querySelectorAll("span")].map((el) => el.textContent).filter((text) => text === IN_SIDEBAR || text === LIBRARY_ONLY);
}

beforeEach(() => {
  vi.useFakeTimers();
  resetRecorder(ipc);
  stubMatchMedia(false);
  stubCanvas();
  stubResizeObserver();
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  footer = document.createElement("div");
  document.body.append(footer);
  launcher = undefined;
});

afterEach(async () => {
  view?.unmount();
  view = undefined;
  footer.remove();
  await drainFrames();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("整合包面板：开始下载后那句说明", () => {
  beforeEach(() => {
    ipc.overrides.set("art_status", () => ({ source: null, panorama: false }));
    ipc.overrides.set("list_manifest", () => ({ latest: { release: "1.21.1", snapshot: "24w14a" }, versions: [], loader_errors: [], stale_since: null, fetch_error: null }));
    ipc.overrides.set("summarize_platform_modpack", () => new Promise(() => {}));
    ipc.overrides.set("install_modpack", () => new Promise(() => {}));
    ipc.overrides.set("set_sidebar_pins", (args: IpcArgs) => args.pins);
  });

  it("入队时侧栏只剩最后一格：新实例占上第 5 格，说法是侧栏与实例库", async () => {
    const pins = EXISTING.slice(0, SIDEBAR_PIN_LIMIT - 1);
    await mountAndInstall(pins);
    expect(api().state.sidebarPins).toEqual([...pins, PACK]);
    expect(backgroundNotes()).toEqual([IN_SIDEBAR]);
  });

  it("入队时侧栏已满 5 个：新实例不在侧栏里，说法只提实例库", async () => {
    await mountAndInstall(EXISTING);
    expect(api().state.sidebarPins).toEqual(EXISTING);
    expect(backgroundNotes()).toEqual([LIBRARY_ONLY]);
  });

  it("入队时已满、下载途中腾出一格：新实例不补进侧栏，说法仍只提实例库", async () => {
    await mountAndInstall(EXISTING);
    await act(() => api().actions.togglePin("魔法"));
    await flush();
    expect(callsOf(ipc, "set_sidebar_pins").map((a) => a.pins)).toEqual([["生存", "创造", "空岛", "机械"]]);
    expect(api().state.sidebarPins).toEqual(["生存", "创造", "空岛", "机械"]);
    expect(backgroundNotes()).toEqual([LIBRARY_ONLY]);
  });
});
