// @vitest-environment jsdom
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { NO_ART, setArtStatus } from "../../mc/art";
import { clearArtCache } from "../../mc/useArt";
import { render, stubMatchMedia, type Rendered } from "../../test/dom";
import { accountHue } from "../illustrations/seed";
import { stubCanvas, type CanvasStub } from "../illustrations/test-canvas";
import { AccountAvatar } from "./AccountAvatar";
import { AccountFigure } from "./AccountFigure";
import { mannequinSkin } from "./mannequin";
import { fallbackDrawn, mannequinFace, mannequinFigure } from "./test-mannequin";
import { preloadSkins } from "./useSkin";

const REMOTE = "aa".repeat(20);

let view: Rendered | undefined;
let canvas: CanvasStub;
let fetchMock: ReturnType<typeof vi.fn>;
let skinSize = { width: 64, height: 64 };
let warn: MockInstance<typeof console.warn>;
const bitmaps: { width: number; height: number; url: string }[] = [];

beforeEach(() => {
  stubMatchMedia(false);
  canvas = stubCanvas();
  vi.stubGlobal("navigator", { userAgent: "Windows" });
  fetchMock = vi.fn(async (url: string) => ({ ok: true, status: 200, blob: async () => new Blob([url]) }));
  vi.stubGlobal("fetch", fetchMock);
  vi.stubGlobal(
    "createImageBitmap",
    vi.fn(async (blob: Blob) => {
      const bitmap = { ...skinSize, url: await blob.text() };
      bitmaps.push(bitmap);
      return bitmap;
    }),
  );
  warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
  skinSize = { width: 64, height: 64 };
  bitmaps.length = 0;
});

afterEach(() => {
  view?.unmount();
  view = undefined;
  setArtStatus(NO_ART);
  clearArtCache();
  vi.unstubAllGlobals();
});

const settle = () => act(() => new Promise<void>((resolve) => setTimeout(resolve, 0)));
const root = () => view?.container.firstElementChild as HTMLElement;

describe("AccountAvatar", () => {
  it("没有皮肤信息时是人偶，不发请求；旁边没有名称时 aria-hidden", async () => {
    view = render(<AccountAvatar uuid="0123456789abcdef0123456789abcdef" skin={null} size={40} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFace("0123456789abcdef0123456789abcdef", 32));
    expect(root().dataset.skArt).toBe("skin");
    expect(root().getAttribute("aria-hidden")).toBe("true");
    expect(root().style.width).toBe("40px");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("默认皮肤在本机没有材质源时不请求，显示人偶", async () => {
    view = render(<AccountAvatar uuid="u1" skin={{ kind: "default", path: "entity/player/wide/steve", model: "wide" }} size={32} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFace("u1", 24));
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("人偶头像是人偶皮肤的正脸叠帽子层，按整数倍放满脸部", async () => {
    const uuid = "mannequin-face-only";
    view = render(<AccountAvatar uuid={uuid} skin={null} size={48} />);
    await settle();
    const { bitmap } = mannequinSkin(accountHue(uuid), "wide");
    expect(canvas.sourceRects(bitmap)).toEqual(["8,8,8,8", "40,8,8,8"]);
    expect(canvas.draws.find((d) => d.source === bitmap)?.canvas).toMatchObject({ width: 40, height: 40 });
  });

  it("默认皮肤取到后换成皮肤正脸 + 帽子层，脸 32 px 按 dpr 1 画满 32 × 32", async () => {
    setArtStatus({ source: { versionId: "1.21.1", degraded: false, legacySkins: false }, panorama: false });
    view = render(<AccountAvatar uuid="u1" skin={{ kind: "default", path: "entity/player/slim/alex", model: "slim" }} size={40} current label="Alex 的头像" />);
    expect(root().dataset.art).toBe("mannequin");
    await settle();
    expect(fetchMock).toHaveBeenCalledWith("http://petalfall-tex.localhost/art/entity/player/slim/alex.png?v=1.21.1", { mode: "cors" });
    expect(root().dataset.art).toBe("skin");
    expect(root().dataset.current).toBe("true");
    expect(root().getAttribute("role")).toBe("img");
    expect(root().getAttribute("aria-label")).toBe("Alex 的头像");
    const skin = bitmaps[0];
    expect(canvas.sourceRects(skin)).toEqual(["8,8,8,8", "40,8,8,8"]);
    const faceDraw = canvas.draws.find((d) => d.source === skin);
    expect(faceDraw?.canvas).toMatchObject({ width: 32, height: 32 });
  });

  it("远程皮肤按 sha1 取；后端判定没有帽子层时只画正脸", async () => {
    view = render(<AccountAvatar uuid="u2" skin={{ kind: "remote", sha1: REMOTE, model: "wide", hat: false }} size={24} />);
    await settle();
    expect(fetchMock).toHaveBeenCalledWith(`http://petalfall-tex.localhost/skin/${REMOTE}.png`, { mode: "cors" });
    expect(root().dataset.art).toBe("skin");
    expect(canvas.sourceRects(bitmaps[0])).toEqual(["8,8,8,8"]);
  });

  it("尺寸不是 64 × 64 / 64 × 32 的皮肤回落人偶并记 warn", async () => {
    skinSize = { width: 64, height: 48 };
    view = render(<AccountAvatar uuid="u3" skin={{ kind: "remote", sha1: "bb".repeat(20), model: "wide", hat: true }} size={32} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFace("u3", 24));
    expect(warn.mock.calls.some((c) => String(c[0]).includes("64x48"))).toBe(true);
  });

  it("下载失败（404）回落人偶", async () => {
    fetchMock.mockImplementation(async () => ({ ok: false, status: 404, blob: async () => new Blob() }));
    view = render(<AccountAvatar uuid="u4" skin={{ kind: "remote", sha1: "cc".repeat(20), model: "wide", hat: true }} size={48} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFace("u4", 40));
  });
});

describe("换皮肤（刷新账户后换了一张）", () => {
  const OLD = { kind: "remote", sha1: "a1".repeat(20), model: "wide", hat: true } as const;
  const NEW = { kind: "remote", sha1: "b2".repeat(20), model: "wide", hat: true } as const;
  /** ArtCrossfade 的叠层：第一层是常驻的人偶，其余是真图层（含正在淡出的）。 */
  const realLayers = () => [...root().querySelectorAll<HTMLElement>('[class*="layer"]')].slice(1);
  let arrive: () => void;

  beforeEach(() => {
    const gate = new Promise<void>((resolve) => {
      arrive = resolve;
    });
    fetchMock.mockImplementation(async (url: string) => {
      if (url.includes(NEW.sha1)) await gate;
      return { ok: true, status: 200, blob: async () => new Blob([url]) };
    });
  });

  it("新皮肤取到之前仍显示旧皮肤，到了两张叠放交叉淡化，中间不落回人偶", async () => {
    view = render(<AccountAvatar uuid="u7" skin={OLD} size={40} />);
    await settle();
    const [oldLayer] = realLayers();
    view.rerender(<AccountAvatar uuid="u7" skin={NEW} size={40} />);
    await settle();
    expect(root().dataset.art).toBe("skin");
    expect(realLayers()).toEqual([oldLayer]);
    expect(bitmaps.map((b) => b.url)).toEqual([`http://petalfall-tex.localhost/skin/${OLD.sha1}.png`]);
    arrive();
    await settle();
    expect(root().dataset.art).toBe("skin");
    const fresh = realLayers().filter((layer) => layer !== oldLayer);
    expect(fresh).toHaveLength(1);
    // 新图层是从透明淡入的那一层（真实计时器下已经走了一点）
    expect(Number(fresh[0].style.opacity)).toBeLessThan(1);
    expect(canvas.sourceRects(bitmaps[1]).length).toBeGreaterThan(0);
  });

  it("新皮肤取不到：等它落定后回落人偶", async () => {
    view = render(<AccountAvatar uuid="u8" skin={OLD} size={40} />);
    await settle();
    fetchMock.mockImplementation(async () => ({ ok: false, status: 404, blob: async () => new Blob() }));
    view.rerender(<AccountAvatar uuid="u8" skin={NEW} size={40} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
  });

  it("账户没有皮肤了：不留旧图，直接回落人偶", async () => {
    view = render(<AccountAvatar uuid="u9" skin={OLD} size={40} />);
    await settle();
    view.rerender(<AccountAvatar uuid="u9" skin={null} size={40} />);
    expect(root().dataset.art).toBe("mannequin");
  });
});

describe("换了账户、皮肤信息还在读（loading）", () => {
  const PREV = { kind: "remote", sha1: "c3".repeat(20), model: "wide", hat: true } as const;
  const NEXT = { kind: "remote", sha1: "d4".repeat(20), model: "slim", hat: true } as const;
  const realLayers = () => [...root().querySelectorAll<HTMLElement>('[class*="layer"]')].slice(1);
  let arrive: () => void;

  beforeEach(() => {
    const gate = new Promise<void>((resolve) => {
      arrive = resolve;
    });
    fetchMock.mockImplementation(async (url: string) => {
      if (url.includes(NEXT.sha1)) await gate;
      return { ok: true, status: 200, blob: async () => new Blob([url]) };
    });
  });

  it("头像：读取中留着上一位的脸、不发请求；读到新皮肤后两张叠放交叉淡化，中间不落回人偶", async () => {
    view = render(<AccountAvatar uuid="prev" skin={PREV} size={40} />);
    await settle();
    const [prevLayer] = realLayers();
    view.rerender(<AccountAvatar uuid="next" skin="loading" size={40} />);
    await settle();
    expect(root().dataset.art).toBe("skin");
    expect(realLayers()).toEqual([prevLayer]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    // 读到了皮肤信息，位图还在取：仍是上一位的脸
    view.rerender(<AccountAvatar uuid="next" skin={NEXT} size={40} />);
    await settle();
    expect([root().dataset.art, realLayers()]).toEqual(["skin", [prevLayer]]);
    arrive();
    await settle();
    const fresh = realLayers().filter((layer) => layer !== prevLayer);
    expect(fresh).toHaveLength(1);
    expect(Number(fresh[0].style.opacity)).toBeLessThan(1);
    expect(bitmaps.map((b) => b.url)).toEqual([`http://petalfall-tex.localhost/skin/${PREV.sha1}.png`, `http://petalfall-tex.localhost/skin/${NEXT.sha1}.png`]);
  });

  it("头像：读到的是「没有皮肤」才回落人偶", async () => {
    view = render(<AccountAvatar uuid="prev" skin={PREV} size={40} />);
    await settle();
    view.rerender(<AccountAvatar uuid="next" skin="loading" size={40} />);
    await settle();
    expect(root().dataset.art).toBe("skin");
    view.rerender(<AccountAvatar uuid="next" skin={null} size={40} />);
    expect(root().dataset.art).toBe("mannequin");
    // 人偶跟着换成新账户的色相
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFace("next", 32));
  });

  it("头像：挂载时就在读、没有上一张可留：先是人偶，不发请求", async () => {
    view = render(<AccountAvatar uuid="next" skin="loading" size={32} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFace("next", 24));
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("立绘：读取中留着上一位的立绘，读到新皮肤后交叉换上", async () => {
    view = render(<AccountFigure uuid="prev" skin={PREV} scale={6} />);
    await settle();
    const [prevLayer] = realLayers();
    view.rerender(<AccountFigure uuid="next" skin="loading" scale={6} />);
    await settle();
    expect([root().dataset.art, realLayers()]).toEqual(["skin", [prevLayer]]);
    view.rerender(<AccountFigure uuid="next" skin={NEXT} scale={6} />);
    arrive();
    await settle();
    expect(root().dataset.art).toBe("skin");
    expect(realLayers().filter((layer) => layer !== prevLayer)).toHaveLength(1);
  });
});

describe("preloadSkins", () => {
  const STEVE = { kind: "default", path: "entity/player/wide/steve", model: "wide" } as const;
  const REMOTE_SKIN = { kind: "remote", sha1: "e5".repeat(20), model: "wide", hat: true } as const;

  it("按与头像相同的地址先取进缓存，之后挂上的头像第一帧就是皮肤", async () => {
    setArtStatus({ source: { versionId: "1.21.1", degraded: false, legacySkins: false }, panorama: false });
    preloadSkins([STEVE, REMOTE_SKIN]);
    await settle();
    expect(fetchMock.mock.calls.map((c) => c[0])).toEqual([
      "http://petalfall-tex.localhost/art/entity/player/wide/steve.png?v=1.21.1",
      `http://petalfall-tex.localhost/skin/${REMOTE_SKIN.sha1}.png`,
    ]);
    view = render(<AccountAvatar uuid="u1" skin={STEVE} size={40} />);
    expect(root().dataset.art).toBe("skin");
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("本机没有材质源时默认皮肤不发请求，远程皮肤照取", async () => {
    preloadSkins([STEVE, REMOTE_SKIN]);
    await settle();
    expect(fetchMock.mock.calls.map((c) => c[0])).toEqual([`http://petalfall-tex.localhost/skin/${REMOTE_SKIN.sha1}.png`]);
  });
});

describe("AccountFigure", () => {
  it("旧格式皮肤：布局方框 scale 5 为 80 × 160，投影画在四周各大 10 px 的外框里，外层只有帽子", async () => {
    skinSize = { width: 64, height: 32 };
    view = render(<AccountFigure uuid="u5" skin={{ kind: "remote", sha1: "dd".repeat(20), model: "wide", hat: true }} scale={5} />);
    await settle();
    expect(root().dataset.art).toBe("skin");
    expect(root().style.width).toBe("80px");
    expect(root().style.height).toBe("160px");
    const frame = root().firstElementChild as HTMLElement;
    expect([frame.style.left, frame.style.top, frame.style.width, frame.style.height]).toEqual(["-10px", "-10px", "100px", "180px"]);
    // 六个部位各三个可见面，帽子再加背面三个、正面三个；旧格式身体与四肢没有外层。垫在下面的人偶也画在同样大的外框上，不算
    const mannequin = mannequinFigure("u5", "wide", 5);
    const faces = canvas.draws.filter((d) => d.canvas !== mannequin && d.canvas.width === 100 && d.canvas.height === 180 && (d.source as { width: number }).width <= 8);
    expect(faces).toHaveLength(6 * 3 + 3 + 3);
    expect(canvas.sourceRects(bitmaps[0])).toEqual(["0,0,64,32"]);
  });

  it("没有皮肤时是人偶的立体立绘，外框和真皮肤同一套", async () => {
    view = render(<AccountFigure uuid="u6" skin={null} scale={4} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFigure("u6", "wide", 4));
    expect(root().style.height).toBe("128px");
    const display = root().querySelector("canvas");
    expect(display?.style.width).toBe("80px");
    expect(display?.style.height).toBe("144px");
  });

  it("细体型的默认皮肤在本机读不到：人偶也是细臂，换上真皮肤时轮廓不跳", async () => {
    view = render(<AccountFigure uuid="u10" skin={{ kind: "default", path: "entity/player/slim/zuri", model: "slim" }} scale={5} />);
    await settle();
    expect(root().dataset.art).toBe("mannequin");
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFigure("u10", "slim", 5));
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("人偶换成默认皮肤：人偶垫在下面，皮肤层从透明淡入，两层叠放交叉", async () => {
    setArtStatus({ source: { versionId: "1.21.1", degraded: false, legacySkins: false }, panorama: false });
    let arrive: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      arrive = resolve;
    });
    fetchMock.mockImplementation(async (url: string) => {
      await gate;
      return { ok: true, status: 200, blob: async () => new Blob([url]) };
    });
    view = render(<AccountFigure uuid="u11" skin={{ kind: "default", path: "entity/player/wide/kai", model: "wide" }} scale={6} />);
    await settle();
    const layers = () => [...root().querySelectorAll<HTMLElement>('[class*="layer"]')];
    expect(root().dataset.art).toBe("mannequin");
    expect(layers()).toHaveLength(1);
    expect(fallbackDrawn(canvas, root())).toBe(mannequinFigure("u11", "wide", 6));
    arrive();
    await settle();
    expect(root().dataset.art).toBe("skin");
    const [under, over] = layers();
    expect(fallbackDrawn(canvas, under)).toBe(mannequinFigure("u11", "wide", 6));
    expect(Number(over.style.opacity)).toBeLessThan(1);
  });
});
