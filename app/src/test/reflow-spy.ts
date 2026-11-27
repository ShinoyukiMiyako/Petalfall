// 组件测试里包住 lib/reflow 的 useReflow：jsdom 不做布局，让位本身测不了，测试只记下它挂到了哪些容器上、走哪条弹簧。
// 挂上、停下与当场补跑的 sync 都照常交给真实现。只被测试引用：vi.mock 的工厂先于静态导入执行，要在工厂里动态 import 它。

import { useMemo } from "react";
import type { SpringName } from "../lib/motion";
import type * as Reflow from "../lib/reflow";

/**
 * 换掉 useReflow 的替身：每挂上一个容器先调 onAttach(容器, 弹簧)，它返回的函数在这一份让位停下之后调用。
 * 返回值与真实现一个形状：带着真实现的 sync，引用只随 spring 变。每次渲染都给新的 ref 的话，React 每次重渲染都先摘下
 * 再挂上，让位被反复重启；丢了 sync，交给 onMorphStart 的就是 undefined，形变起步时不当场补跑。
 */
export function spyUseReflow(real: typeof Reflow, onAttach: (el: HTMLElement, spring: SpringName) => void | (() => void)): typeof Reflow.useReflow {
  return function useReflow<T extends HTMLElement>(spring: SpringName = "settle") {
    const inner = real.useReflow<T>(spring);
    return useMemo(
      () =>
        Object.assign(
          (el: T | null) => {
            if (el === null) return;
            const detach = onAttach(el, spring);
            const stop = inner(el);
            if (typeof stop !== "function") throw new Error("useReflow 挂上之后没有给出停止函数");
            return () => {
              stop();
              if (typeof detach === "function") detach();
            };
          },
          { sync: inner.sync },
        ),
      [inner, spring],
    );
  };
}
