// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { assertAnimationFrameTimerAlive } from "./frame-timer";

// 测试文件加载时还在真实计时器上，窗口上是 jsdom 自己的那一个
const requestFrame = window.requestAnimationFrame;
const cancelFrame = window.cancelAnimationFrame;

describe("assertAnimationFrameTimerAlive", () => {
  it("没有回调排着：照常通过，探测排的那一个随即撤掉，换上假时钟后新排的帧推一帧就到", async () => {
    await assertAnimationFrameTimerAlive();
    vi.useFakeTimers();
    const next = vi.fn();
    requestFrame.call(window, next);
    await vi.advanceTimersByTimeAsync(16);
    expect(next).toHaveBeenCalledTimes(1);
  });

  it("真实计时器上已经排着一帧：等它到点，不报错", async () => {
    const pending = vi.fn();
    requestFrame.call(window, pending);
    await assertAnimationFrameTimerAlive();
    expect(pending).toHaveBeenCalledTimes(1);
  });

  it("卸下假时钟时还有动画帧回调排着：报出计时器在假时钟切换时丢了；撤掉那个回调后又照常通过", async () => {
    vi.useFakeTimers();
    const lost = requestFrame.call(window, () => undefined);
    vi.useRealTimers();
    try {
      await expect(assertAnimationFrameTimerAlive()).rejects.toThrow("jsdom 的动画帧计时器在假时钟切换时丢了");
    } finally {
      // 计数回到 0，用例结束时 test/setup 的同一道检查不受这里造出来的状态牵连
      cancelFrame.call(window, lost);
    }
    await assertAnimationFrameTimerAlive();
  });

  it("假定时器下直接报错，不拿没推进的假时钟冒充计时器丢了", async () => {
    vi.useFakeTimers();
    await expect(assertAnimationFrameTimerAlive()).rejects.toThrow("要在真实计时器下用");
  });
});
