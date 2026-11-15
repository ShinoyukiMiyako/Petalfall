// @vitest-environment jsdom
// test/setup.ts 的全局收尾横跨用例起作用：第一个用例故意不收尾就结束，后面的用例检查它留下的东西都收走了。
// 同一文件里的用例按书写顺序依次跑，后面的用例依赖第一个。
import { motion } from "framer-motion";
import { useEffect } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render, stepFrames } from "./dom";

const unmounted: string[] = [];
/** 文件自己的 afterEach 跑的那一刻：是否还在假定时器上、body 里还剩几个渲染容器。 */
const seenByOwnTeardown: { fake: boolean; containers: number }[] = [];

afterEach(() => {
  seenByOwnTeardown.push({ fake: vi.isFakeTimers(), containers: document.body.childElementCount });
});

function Fade({ id, seconds }: { id: string; seconds: number }) {
  useEffect(
    () => () => {
      unmounted.push(id);
    },
    [id],
  );
  return <motion.div data-testid={id} initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={{ duration: seconds, ease: "linear" }} />;
}

function opacity(id: string): number {
  const el = document.querySelector<HTMLElement>(`[data-testid="${id}"]`);
  if (el === null) throw new Error(`没有渲染出动画元素 ${id}`);
  return Number(el.style.opacity);
}

describe("用例结束后的全局收尾", () => {
  it("假时钟上播到一半就结束：不卸载，也不换回真实计时器", async () => {
    vi.useFakeTimers();
    render(<Fade id="left" seconds={5} />);
    await stepFrames(2);
    expect(opacity("left")).toBeGreaterThan(0);
    expect(opacity("left")).toBeLessThan(1);
  });

  it("留下的组件在文件自己的 afterEach 之前已卸载、容器已移除；假定时器等它跑完才换回真实计时器", () => {
    expect(unmounted).toEqual(["left"]);
    expect(document.body.childElementCount).toBe(0);
    expect(seenByOwnTeardown).toEqual([{ fake: true, containers: 0 }]);
    expect(vi.isFakeTimers()).toBe(false);
  });

  it("排在旧假时钟上的帧换计时器之前已排空：换上新的假时钟，新挂的动画头两步就推得动", async () => {
    vi.useFakeTimers();
    render(<Fade id="next" seconds={0.16} />);
    await stepFrames(2);
    expect(opacity("next")).toBeGreaterThan(0);
    expect(unmounted).toEqual(["left"]);
  });
});
