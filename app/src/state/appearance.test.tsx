// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Appearance } from "../domain/types";
import { getAppearance, setAppearance as persist, type AppearanceDto, type AppearancePatchDto } from "../ipc/appearance";
import { render, stubMatchMedia, stubMediaQueries, type Rendered } from "../test/dom";
import {
  AppearanceProvider,
  appearanceFromDto,
  appearancePatchToDto,
  applyAppearance,
  DEFAULT_APPEARANCE,
  INSTANT_THEME_ATTR,
  loadAppearance,
  resolveMode,
  useAppearance,
  type AppearanceApi,
  type SystemPreferences,
} from "./appearance";

vi.mock("../ipc/appearance", () => ({ getAppearance: vi.fn(), setAppearance: vi.fn() }));

const html = document.documentElement;
const DAY_DTO: AppearanceDto = { mode: "day", accent: "sakura", petals: true, panorama: false, reduce_motion: false, cover_background: "texture" };
/** 系统浅色、没开减少动态效果。 */
const LIGHT_SYSTEM: SystemPreferences = { reducedMotion: false, dark: false };
const REDUCED = "(prefers-reduced-motion: reduce)";
const DARK = "(prefers-color-scheme: dark)";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** 后端行为：把补丁合并进已确认的状态后返回。 */
function backendMerging(start: AppearanceDto) {
  let state = { ...start };
  return (patch: AppearancePatchDto) => {
    state = { ...state, ...patch };
    return Promise.resolve({ ...state });
  };
}

let api: AppearanceApi;
function Probe() {
  api = useAppearance();
  return null;
}

let view: Rendered | undefined;
beforeEach(() => {
  vi.clearAllMocks();
  for (const attr of ["data-sk-mode", "data-sk-accent", "data-sk-reduced"]) html.removeAttribute(attr);
});
afterEach(() => {
  view?.unmount();
  view = undefined;
  vi.unstubAllGlobals();
});

describe("DTO 转换", () => {
  it("snake_case 转成领域字段", () => {
    expect(appearanceFromDto({ mode: "night", accent: "moss", petals: false, panorama: true, reduce_motion: true, cover_background: "art" })).toEqual({
      mode: "night",
      accent: "moss",
      petals: false,
      panorama: true,
      reduceMotion: true,
      coverBackground: "art",
    });
  });

  it("未知取值直接抛错", () => {
    expect(() => appearanceFromDto({ ...DAY_DTO, cover_background: "tiles" as never })).toThrow("未知的卡面默认背景：tiles");
    expect(() => appearanceFromDto({ ...DAY_DTO, accent: "rose" as never })).toThrow("未知的强调色：rose");
    expect(() => appearanceFromDto({ ...DAY_DTO, mode: "dusk" as never })).toThrow("未知的外观明暗：dusk");
    expect(() => appearanceFromDto({ ...DAY_DTO, petals: "yes" as never })).toThrow("外观字段 petals 应为布尔值");
  });

  it("补丁只写出出现的字段", () => {
    expect(appearancePatchToDto({ reduceMotion: true })).toEqual({ reduce_motion: true });
    expect(appearancePatchToDto({ mode: "night", accent: "amethyst" })).toEqual({ mode: "night", accent: "amethyst" });
    expect(appearancePatchToDto({})).toEqual({});
    expect(appearancePatchToDto({ coverBackground: "art" })).toEqual({ cover_background: "art" });
    expect(() => appearancePatchToDto({ accent: "teal" as never })).toThrow("未知的强调色：teal");
  });

  it("跟随系统原样进出：读进来是 system，补丁写出去也是 system，不在这里换成日樱或夜樱", () => {
    expect(appearanceFromDto({ ...DAY_DTO, mode: "system" }).mode).toBe("system");
    expect(appearancePatchToDto({ mode: "system" })).toEqual({ mode: "system" });
  });

  it("loadAppearance 读后端并校验", async () => {
    vi.mocked(getAppearance).mockResolvedValueOnce({ ...DAY_DTO, accent: "amethyst" });
    expect(await loadAppearance()).toEqual({ ...DEFAULT_APPEARANCE, accent: "amethyst" });
    vi.mocked(getAppearance).mockResolvedValueOnce({ ...DAY_DTO, mode: "noon" as never });
    await expect(loadAppearance()).rejects.toThrow("未知的外观明暗：noon");
  });
});

describe("resolveMode", () => {
  it.each([
    ["day", false, "day"],
    ["day", true, "day"],
    ["night", false, "night"],
    ["night", true, "night"],
    ["system", false, "day"],
    ["system", true, "night"],
  ] as const)("偏好 %s、系统深色 %s -> 画 %s", (mode, dark, shown) => {
    expect(resolveMode(mode, dark)).toBe(shown);
  });

  it("未知的明暗直接抛错，不回落成日樱", () => {
    expect(() => resolveMode("auto" as never, true)).toThrow("未知的外观明暗：auto");
  });
});

describe("applyAppearance", () => {
  it("跟随系统：data-sk-mode 写系统此刻的日樱或夜樱，从不写 system", () => {
    applyAppearance({ ...DEFAULT_APPEARANCE, mode: "system" }, { reducedMotion: false, dark: true });
    expect(html.getAttribute("data-sk-mode")).toBe("night");
    applyAppearance({ ...DEFAULT_APPEARANCE, mode: "system" }, LIGHT_SYSTEM);
    expect(html.getAttribute("data-sk-mode")).toBe("day");
  });

  it("明暗与强调色永远显式写出", () => {
    applyAppearance(DEFAULT_APPEARANCE, LIGHT_SYSTEM);
    expect(html.getAttribute("data-sk-mode")).toBe("day");
    expect(html.getAttribute("data-sk-accent")).toBe("sakura");
    expect(html.hasAttribute("data-sk-reduced")).toBe(false);
  });

  it.each([
    [false, false, false],
    [true, false, true],
    [false, true, true],
    [true, true, true],
  ])("应用设置 %s、系统偏好 %s -> 减少动画 %s", (setting, system, reduced) => {
    applyAppearance({ ...DEFAULT_APPEARANCE, reduceMotion: setting }, { reducedMotion: system, dark: false });
    expect(html.hasAttribute("data-sk-reduced")).toBe(reduced);
  });

  describe("换明暗与强调色时颜色一帧切完", () => {
    /** 这一次写入里 <html> 属性的变化次序；关过渡的标记记成「+标记」「-标记」。 */
    const writes = (apply: () => void): string[] => {
      const observer = new MutationObserver(() => undefined);
      observer.observe(html, { attributes: true, attributeOldValue: true });
      apply();
      const records = observer.takeRecords();
      observer.disconnect();
      return records.map((r) => (r.attributeName === INSTANT_THEME_ATTR ? `${r.oldValue === null ? "+" : "-"}${INSTANT_THEME_ATTR}` : String(r.attributeName)));
    };

    it.each([
      ["明暗", { ...DEFAULT_APPEARANCE, mode: "night" }],
      ["强调色", { ...DEFAULT_APPEARANCE, accent: "moss" }],
    ] as const)("换%s：先挂上关过渡的标记，改完属性再撤掉", (_name, next) => {
      applyAppearance(DEFAULT_APPEARANCE, LIGHT_SYSTEM);
      expect(writes(() => applyAppearance(next, LIGHT_SYSTEM))).toEqual([`+${INSTANT_THEME_ATTR}`, "data-sk-mode", "data-sk-accent", `-${INSTANT_THEME_ATTR}`]);
      expect(html.hasAttribute(INSTANT_THEME_ATTR)).toBe(false);
    });

    it("跟随系统时系统换了深浅色：与手动换明暗一样先关过渡；画出来的明暗没变就不关", () => {
      const following = { ...DEFAULT_APPEARANCE, mode: "system" } as const;
      applyAppearance(following, LIGHT_SYSTEM);
      expect(writes(() => applyAppearance(following, { reducedMotion: false, dark: true }))).toEqual([`+${INSTANT_THEME_ATTR}`, "data-sk-mode", "data-sk-accent", `-${INSTANT_THEME_ATTR}`]);
      expect(html.getAttribute("data-sk-mode")).toBe("night");
      // 日樱换成跟随系统、系统正好是浅色：颜色没变，不关过渡
      applyAppearance(DEFAULT_APPEARANCE, LIGHT_SYSTEM);
      expect(writes(() => applyAppearance(following, LIGHT_SYSTEM))).toEqual(["data-sk-mode", "data-sk-accent"]);
    });

    it("启动时第一次写入、只改减少动画：不关过渡", () => {
      expect(writes(() => applyAppearance(DEFAULT_APPEARANCE, LIGHT_SYSTEM))).toEqual(["data-sk-mode", "data-sk-accent"]);
      expect(writes(() => applyAppearance({ ...DEFAULT_APPEARANCE, reduceMotion: true }, LIGHT_SYSTEM))).toEqual(["data-sk-mode", "data-sk-accent", "data-sk-reduced"]);
    });

    it("全局样式里有这个标记关掉全部过渡的规则", () => {
      const css = readFileSync(resolve(__dirname, "../styles/base.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
      const rule = new RegExp(`:root\\[${INSTANT_THEME_ATTR}\\] \\*,[^{]*\\{([^}]*)\\}`).exec(css);
      if (rule === null) throw new Error(`base.css 里没有 ${INSTANT_THEME_ATTR} 的规则`);
      expect(rule[1].trim()).toBe("transition: none !important;");
    });
  });
});

describe("AppearanceProvider", () => {
  const mount = (initial: Appearance = DEFAULT_APPEARANCE) => {
    view = render(
      <AppearanceProvider initial={initial}>
        <Probe />
      </AppearanceProvider>,
    );
  };

  it("在 Provider 之外使用直接抛错", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    expect(() => render(<Probe />)).toThrow("useAppearance 必须在 AppearanceProvider 之内使用");
  });

  it("挂载时按初始值与系统偏好写 <html>", () => {
    stubMatchMedia(true);
    mount({ ...DEFAULT_APPEARANCE, mode: "night", accent: "moss" });
    expect(html.getAttribute("data-sk-mode")).toBe("night");
    expect(html.getAttribute("data-sk-accent")).toBe("moss");
    expect(html.hasAttribute("data-sk-reduced")).toBe(true);
    expect(api.reduced).toBe(true);
  });

  it("先同步改 <html>，后端确认后以确认值为准", async () => {
    stubMatchMedia(false);
    const pending = deferred<AppearanceDto>();
    vi.mocked(persist).mockReturnValueOnce(pending.promise);
    mount();
    let done!: Promise<void>;
    act(() => {
      done = api.setAppearance({ mode: "night" });
    });
    expect(html.getAttribute("data-sk-mode")).toBe("night");
    expect(api.appearance.mode).toBe("night");
    expect(vi.mocked(persist).mock.calls).toEqual([[{ mode: "night" }]]);
    await act(async () => {
      pending.resolve({ ...DAY_DTO, mode: "night", accent: "amethyst" });
      await done;
    });
    expect(html.getAttribute("data-sk-accent")).toBe("amethyst");
    expect(api.appearance).toEqual({ ...DEFAULT_APPEARANCE, mode: "night", accent: "amethyst" });
  });

  it("写入失败：界面回到后端最后确认的值，原错误抛给调用方", async () => {
    stubMatchMedia(false);
    vi.mocked(persist).mockImplementationOnce(backendMerging(DAY_DTO));
    mount();
    await act(() => api.setAppearance({ accent: "moss" }));
    expect(html.getAttribute("data-sk-accent")).toBe("moss");

    const failure = new Error("配置文件被占用");
    vi.mocked(persist).mockRejectedValueOnce(failure);
    let caught: unknown;
    await act(async () => {
      await api.setAppearance({ accent: "amethyst", mode: "night" }).catch((e: unknown) => {
        caught = e;
      });
    });
    expect(caught).toBe(failure);
    expect(html.getAttribute("data-sk-accent")).toBe("moss");
    expect(html.getAttribute("data-sk-mode")).toBe("day");
    expect(api.appearance).toEqual({ ...DEFAULT_APPEARANCE, accent: "moss" });
  });

  it("连续切换：只有最后一次请求决定界面，早到或晚到的旧结果都不改回去", async () => {
    stubMatchMedia(false);
    const first = deferred<AppearanceDto>();
    const second = deferred<AppearanceDto>();
    vi.mocked(persist).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    mount();
    let p1!: Promise<void>;
    let p2!: Promise<void>;
    act(() => {
      p1 = api.setAppearance({ accent: "amethyst" });
    });
    act(() => {
      p2 = api.setAppearance({ accent: "moss" });
    });
    expect(html.getAttribute("data-sk-accent")).toBe("moss");
    await act(async () => {
      second.resolve({ ...DAY_DTO, accent: "moss" });
      await p2;
    });
    await act(async () => {
      first.resolve({ ...DAY_DTO, accent: "amethyst" });
      await p1;
    });
    expect(html.getAttribute("data-sk-accent")).toBe("moss");
    expect(api.appearance.accent).toBe("moss");
  });

  it("较早的请求比更新的那次晚确认：确认值仍是更新的那次，之后的写入失败回到它而不是较早的那次", async () => {
    stubMatchMedia(false);
    const first = deferred<AppearanceDto>();
    const second = deferred<AppearanceDto>();
    vi.mocked(persist).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    mount();
    let p1!: Promise<void>;
    let p2!: Promise<void>;
    act(() => {
      p1 = api.setAppearance({ accent: "amethyst" });
    });
    act(() => {
      p2 = api.setAppearance({ accent: "moss" });
    });
    await act(async () => {
      second.resolve({ ...DAY_DTO, accent: "moss" });
      await p2;
    });
    await act(async () => {
      first.resolve({ ...DAY_DTO, accent: "amethyst" });
      await p1;
    });
    expect(html.getAttribute("data-sk-accent")).toBe("moss");

    const failure = new Error("配置文件被占用");
    vi.mocked(persist).mockRejectedValueOnce(failure);
    let caught: unknown;
    await act(async () => {
      await api.setAppearance({ mode: "night" }).catch((e: unknown) => {
        caught = e;
      });
    });
    expect(caught).toBe(failure);
    expect(html.getAttribute("data-sk-accent")).toBe("moss");
    expect(html.getAttribute("data-sk-mode")).toBe("day");
    expect(api.appearance).toEqual({ ...DEFAULT_APPEARANCE, accent: "moss" });
  });

  it("新请求先失败、旧请求随后确认：界面落到旧请求确认的后端状态，不停在更早的值上", async () => {
    stubMatchMedia(false);
    const first = deferred<AppearanceDto>();
    const second = deferred<AppearanceDto>();
    vi.mocked(persist).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    mount();
    let p1!: Promise<void>;
    let p2!: Promise<void>;
    act(() => {
      p1 = api.setAppearance({ accent: "amethyst" });
      p2 = api.setAppearance({ mode: "night" });
    });
    const failure = new Error("写入失败");
    await act(async () => {
      second.reject(failure);
      await expect(p2).rejects.toBe(failure);
    });
    expect(html.getAttribute("data-sk-mode")).toBe("day");
    await act(async () => {
      first.resolve({ ...DAY_DTO, accent: "amethyst" });
      await p1;
    });
    expect(html.getAttribute("data-sk-accent")).toBe("amethyst");
    expect(html.getAttribute("data-sk-mode")).toBe("day");
    expect(api.appearance).toEqual({ ...DEFAULT_APPEARANCE, accent: "amethyst" });
  });

  it("旧请求确认时更新的请求还在途：不提前换成旧请求的确认值，等更新的那次", async () => {
    stubMatchMedia(false);
    const first = deferred<AppearanceDto>();
    const second = deferred<AppearanceDto>();
    vi.mocked(persist).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    mount();
    let p1!: Promise<void>;
    let p2!: Promise<void>;
    act(() => {
      p1 = api.setAppearance({ accent: "amethyst" });
      p2 = api.setAppearance({ accent: "moss" });
    });
    await act(async () => {
      first.resolve({ ...DAY_DTO, accent: "amethyst" });
      await p1;
    });
    expect(html.getAttribute("data-sk-accent")).toBe("moss");
    await act(async () => {
      second.resolve({ ...DAY_DTO, accent: "moss" });
      await p2;
    });
    expect(api.appearance).toEqual({ ...DEFAULT_APPEARANCE, accent: "moss" });
  });

  it("旧请求失败时新请求仍在途，不回滚新请求的乐观值", async () => {
    stubMatchMedia(false);
    const first = deferred<AppearanceDto>();
    const second = deferred<AppearanceDto>();
    vi.mocked(persist).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    mount();
    let p1!: Promise<void>;
    let p2!: Promise<void>;
    act(() => {
      p1 = api.setAppearance({ mode: "night" });
      p2 = api.setAppearance({ accent: "moss" });
    });
    const failure = new Error("写入失败");
    await act(async () => {
      first.reject(failure);
      await expect(p1).rejects.toBe(failure);
    });
    expect(html.getAttribute("data-sk-mode")).toBe("night");
    expect(html.getAttribute("data-sk-accent")).toBe("moss");
    await act(async () => {
      second.resolve({ ...DAY_DTO, mode: "night", accent: "moss" });
      await p2;
    });
    expect(api.appearance).toEqual({ ...DEFAULT_APPEARANCE, mode: "night", accent: "moss" });
  });

  it("非法补丁在发请求之前抛错，界面不动", async () => {
    stubMatchMedia(false);
    mount();
    await expect(api.setAppearance({ mode: "dusk" as never })).rejects.toThrow("未知的外观明暗：dusk");
    expect(persist).not.toHaveBeenCalled();
    expect(html.getAttribute("data-sk-mode")).toBe("day");
  });

  it("系统减少动画偏好变化时实时跟随，卸载后退订", () => {
    const media = stubMediaQueries({ [REDUCED]: false, [DARK]: false });
    mount();
    expect(api.reduced).toBe(false);
    media[REDUCED].set(true);
    expect(api.reduced).toBe(true);
    expect(html.hasAttribute("data-sk-reduced")).toBe(true);
    media[REDUCED].set(false);
    expect(html.hasAttribute("data-sk-reduced")).toBe(false);
    expect([media[REDUCED].listenerCount(), media[DARK].listenerCount()]).toEqual([1, 1]);
    view?.unmount();
    view = undefined;
    expect([media[REDUCED].listenerCount(), media[DARK].listenerCount()]).toEqual([0, 0]);
  });

  describe("跟随系统", () => {
    it("默认仍是日樱：系统是深色时，没选跟随系统的外观照样画日樱", () => {
      expect(DEFAULT_APPEARANCE.mode).toBe("day");
      const media = stubMediaQueries({ [REDUCED]: false, [DARK]: true });
      mount();
      expect(html.getAttribute("data-sk-mode")).toBe("day");
      expect(api.resolvedMode).toBe("day");
      media[DARK].set(false);
      media[DARK].set(true);
      expect(html.getAttribute("data-sk-mode")).toBe("day");
      expect(api.resolvedMode).toBe("day");
    });

    it("三档切换：日樱、夜樱、跟随系统各写一次后端，<html> 画出对应的明暗，确认值留着 system", async () => {
      stubMediaQueries({ [REDUCED]: false, [DARK]: false });
      vi.mocked(persist).mockImplementation(backendMerging(DAY_DTO));
      mount();
      await act(() => api.setAppearance({ mode: "night" }));
      expect([html.getAttribute("data-sk-mode"), api.appearance.mode, api.resolvedMode]).toEqual(["night", "night", "night"]);
      await act(() => api.setAppearance({ mode: "system" }));
      // 系统是浅色：跟随系统画日樱，偏好仍记作 system
      expect([html.getAttribute("data-sk-mode"), api.appearance.mode, api.resolvedMode]).toEqual(["day", "system", "day"]);
      await act(() => api.setAppearance({ mode: "day" }));
      expect([html.getAttribute("data-sk-mode"), api.appearance.mode, api.resolvedMode]).toEqual(["day", "day", "day"]);
      expect(vi.mocked(persist).mock.calls).toEqual([[{ mode: "night" }], [{ mode: "system" }], [{ mode: "day" }]]);
    });

    it("跟随系统时系统换深浅色：<html> 当场换明暗，resolvedMode 跟着变，不用重新载入；改回日樱后不再跟着变", async () => {
      const media = stubMediaQueries({ [REDUCED]: false, [DARK]: false });
      vi.mocked(persist).mockImplementation(backendMerging({ ...DAY_DTO, mode: "system" }));
      mount({ ...DEFAULT_APPEARANCE, mode: "system" });
      expect([html.getAttribute("data-sk-mode"), api.resolvedMode]).toEqual(["day", "day"]);
      media[DARK].set(true);
      expect([html.getAttribute("data-sk-mode"), api.resolvedMode]).toEqual(["night", "night"]);
      expect(html.hasAttribute(INSTANT_THEME_ATTR)).toBe(false);
      media[DARK].set(false);
      expect([html.getAttribute("data-sk-mode"), api.resolvedMode]).toEqual(["day", "day"]);
      expect(api.appearance.mode).toBe("system");

      await act(() => api.setAppearance({ mode: "day" }));
      media[DARK].set(true);
      expect([html.getAttribute("data-sk-mode"), api.resolvedMode]).toEqual(["day", "day"]);
      expect(persist).toHaveBeenCalledTimes(1);
    });

    it("切到跟随系统时用的是系统此刻的明暗：不跟随期间系统变过，切过去当场画对", async () => {
      const media = stubMediaQueries({ [REDUCED]: false, [DARK]: false });
      vi.mocked(persist).mockImplementation(backendMerging(DAY_DTO));
      mount();
      media[DARK].set(true);
      expect(html.getAttribute("data-sk-mode")).toBe("day");
      let done!: Promise<void>;
      act(() => {
        done = api.setAppearance({ mode: "system" });
      });
      // 乐观值同一帧画上，不等后端确认
      expect(html.getAttribute("data-sk-mode")).toBe("night");
      await act(() => done);
      expect(api.resolvedMode).toBe("night");
    });

    it("写入跟随系统失败：回到后端确认的明暗，系统之后再变也不跟", async () => {
      const media = stubMediaQueries({ [REDUCED]: false, [DARK]: true });
      const failure = new Error("配置文件被占用");
      vi.mocked(persist).mockRejectedValueOnce(failure);
      mount();
      let caught: unknown;
      await act(async () => {
        await api.setAppearance({ mode: "system" }).catch((e: unknown) => {
          caught = e;
        });
      });
      expect(caught).toBe(failure);
      expect([html.getAttribute("data-sk-mode"), api.appearance.mode]).toEqual(["day", "day"]);
      media[DARK].set(false);
      media[DARK].set(true);
      expect(html.getAttribute("data-sk-mode")).toBe("day");
    });
  });

  it("应用设置开启减少动画时，系统偏好关闭也保持减少", async () => {
    const media = stubMatchMedia(false);
    vi.mocked(persist).mockImplementationOnce(backendMerging(DAY_DTO));
    mount();
    await act(() => api.setAppearance({ reduceMotion: true }));
    expect(html.hasAttribute("data-sk-reduced")).toBe(true);
    media.set(false);
    expect(html.hasAttribute("data-sk-reduced")).toBe(true);
    expect(api.reduced).toBe(true);
  });
});
