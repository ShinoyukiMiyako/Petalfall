// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, stubMatchMedia, type MediaQueryStub, type Rendered } from "../../test/dom";
import { BlossomBranch, BRANCH_CACHE_LIMIT } from "./BlossomBranch";
import { paintBranch, rasterizeBranch } from "./branch-art";
import { stubCanvas, type CanvasStub } from "./test-canvas";

describe("BlossomBranch", () => {
  let view: Rendered | undefined;
  let canvases: CanvasStub;
  let media: MediaQueryStub;

  beforeEach(() => {
    media = stubMatchMedia(false);
    canvases = stubCanvas();
  });

  afterEach(() => {
    view?.unmount();
    view = undefined;
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  function show(width: number, height: number, dpr: number, extra: { mirror?: boolean; dim?: boolean; className?: string } = {}) {
    vi.stubGlobal("devicePixelRatio", dpr);
    view = render(<BlossomBranch width={width} height={height} {...extra} />);
    const frame = view.container.firstElementChild as HTMLElement;
    const canvas = frame.querySelector("canvas");
    if (canvas === null) throw new Error("BlossomBranch 没有渲染 <canvas>");
    return { frame, canvas };
  }

  // 框、缩放比、每格设备像素、格数
  it.each([
    [248, 200, 1, 3, 82, 66],
    [248, 200, 1.25, 4, 77, 62],
    [248, 200, 1.5, 5, 74, 60],
    [248, 200, 2, 6, 82, 66],
    [150, 96, 1.25, 4, 46, 30],
    [272, 200, 1.75, 5, 95, 70],
  ])("%d x %d、缩放比 %d：每格 %d 设备像素，%d x %d 格，位图一比一贴上", (width, height, dpr, cell, cols, rows) => {
    const { frame, canvas } = show(width, height, dpr);

    expect(frame.style.width).toBe(`${width}px`);
    expect(frame.style.height).toBe(`${height}px`);
    expect([canvas.width, canvas.height]).toEqual([cols * cell, rows * cell]);
    expect(Number.parseFloat(canvas.style.width) * dpr).toBeCloseTo(cols * cell, 6);
    expect(Number.parseFloat(canvas.style.height) * dpr).toBeCloseTo(rows * cell, 6);

    expect(canvases.draws).toHaveLength(1);
    const { source, args, canvas: target } = canvases.draws[0] as (typeof canvases.draws)[number] & { source: OffscreenCanvas };
    expect(target).toBe(canvas);
    expect(args).toEqual([0, 0]);
    expect([source.width, source.height]).toEqual([cols * cell, rows * cell]);
    const put = source.getContext("2d")?.putImageData as unknown as { mock: { calls: [ImageData, number, number][] } };
    expect(put.mock.calls).toHaveLength(1);
    const expected = rasterizeBranch(paintBranch(width, height, cell / dpr), cell);
    expect([...put.mock.calls[0][0].data]).toEqual([...expected.data]);
  });

  it("窗口拖到另一块缩放不同的屏幕后按新的格子重画", () => {
    const { canvas } = show(210, 150, 1);
    expect(canvas.width).toBe(70 * 3);

    vi.stubGlobal("devicePixelRatio", 2);
    media.set(true);

    expect(canvas.width).toBe(70 * 6);
    const last = canvases.draws[canvases.draws.length - 1] as { source: OffscreenCanvas };
    expect(last.source.width).toBe(70 * 6);
  });

  it("尺寸不变时重渲染不重新生成位图", () => {
    show(210, 150, 1);
    view?.rerender(<BlossomBranch width={210} height={150} className="另一个" />);
    expect(new Set(canvases.draws.map((draw) => draw.source)).size).toBe(1);
  });

  /** 位图里写像素的次数：每生成一幅写一次。 */
  const puts = (source: unknown) => (source as OffscreenCanvas).getContext("2d")?.putImageData as unknown as { mock: { calls: unknown[] } };

  // 缓存是模块级的，跨用例留着：各用例取别处没用过的尺寸，第一次画都是现画
  it("同一尺寸的几幅（账户页的几张账户卡）共用一张位图，只画一次；镜像与半透明不同也共用", () => {
    vi.stubGlobal("devicePixelRatio", 1.25);
    const width = 180 + Math.floor(Math.random() * 20);
    view = render(
      <>
        <BlossomBranch width={width} height={140} dim />
        <BlossomBranch width={width} height={140} dim />
        <BlossomBranch width={width} height={140} mirror />
      </>,
    );
    expect(canvases.draws).toHaveLength(3);
    const sources = new Set(canvases.draws.map((draw) => draw.source));
    expect(sources.size).toBe(1);
    const [source] = sources;
    expect(puts(source).mock.calls).toHaveLength(1);
    // 换个尺寸照常现画
    view.unmount();
    view = render(<BlossomBranch width={width} height={141} dim />);
    expect(canvases.draws).toHaveLength(4);
    expect(canvases.draws[3].source).not.toBe(source);
  });

  it("卸载之后再挂上同一尺寸：不重画，直接贴上次的位图", () => {
    const first = show(232, 150, 1.5);
    const source = canvases.draws[0].source;
    view?.unmount();
    view = undefined;
    const again = show(232, 150, 1.5);
    expect(again.canvas).not.toBe(first.canvas);
    expect(canvases.draws).toHaveLength(2);
    expect(canvases.draws[1].source).toBe(source);
    expect(puts(source).mock.calls).toHaveLength(1);
  });

  it(`缓存至多留 ${BRANCH_CACHE_LIMIT} 幅：再多画一种尺寸，最久没用的那幅被挤掉，再要时重画`, () => {
    vi.stubGlobal("devicePixelRatio", 1);
    const sizes = Array.from({ length: BRANCH_CACHE_LIMIT + 1 }, (_, i) => 60 + i);
    for (const width of sizes) {
      view = render(<BlossomBranch width={width} height={30} />);
      view.unmount();
    }
    view = undefined;
    const firstOf = (width: number) => canvases.draws[sizes.indexOf(width)].source;
    // 第二种尺寸还在：贴的是同一张
    view = render(<BlossomBranch width={sizes[1]} height={30} />);
    expect(canvases.draws.at(-1)?.source).toBe(firstOf(sizes[1]));
    view.unmount();
    // 第一种尺寸已被挤掉：重画一张新的
    view = render(<BlossomBranch width={sizes[0]} height={30} />);
    const redrawn = canvases.draws.at(-1)?.source;
    expect(redrawn).not.toBe(firstOf(sizes[0]));
    expect(puts(redrawn).mock.calls).toHaveLength(1);
  });

  it("镜像、半透明与类名落在外框上，纯装饰对读屏隐藏", () => {
    const { frame } = show(150, 96, 1, { mirror: true, dim: true, className: "外部类" });
    expect(frame.dataset.mirror).toBe("true");
    expect(frame.dataset.dim).toBe("true");
    expect(frame.classList.contains("外部类")).toBe(true);
    expect(frame.getAttribute("aria-hidden")).toBe("true");

    view?.rerender(<BlossomBranch width={150} height={96} />);
    expect(frame.dataset.mirror).toBeUndefined();
    expect(frame.dataset.dim).toBeUndefined();
  });

  it("尺寸不是正数时抛错", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    vi.stubGlobal("devicePixelRatio", 1);
    expect(() => render(<BlossomBranch width={0} height={96} />)).toThrow("花枝插画尺寸必须为正：0x96");
  });
});
