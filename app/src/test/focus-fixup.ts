// jsdom 没有实现 HTML 的 focus fixup：聚焦着的按钮被原生禁用后，浏览器把焦点落回 body，
// jsdom 却让 activeElement 停在原处，于是「改用原生禁用」的写法在测试里也会通过。
// 测「焦点留在按钮上」时打开这个替身，按浏览器的样子把焦点落回 body。只被 *.test.tsx 引用。
// jsdom 也不算可见性：藏在 visibility: hidden 里的元素照样能聚焦，见 refuseHiddenFocus。

import { vi } from "vitest";

/** 开始模拟；返回值停止模拟。属性变化经 MutationObserver 在微任务里处理，断言前要先让出一次微任务。 */
export function emulateFocusFixup(): () => void {
  const observer = new MutationObserver(() => {
    const active = document.activeElement;
    if (!(active instanceof HTMLButtonElement) || !active.disabled) return;
    // jsdom 的 blur() 对不可聚焦的元素不起作用：先放开禁用让它失焦，随即还原
    active.disabled = false;
    active.blur();
    active.disabled = true;
  });
  observer.observe(document.body, { subtree: true, attributes: true, attributeFilter: ["disabled"] });
  return () => observer.disconnect();
}

export interface FocusCall {
  target: HTMLElement;
  /** 调用那一刻元素自己或某个祖先的行内样式是 visibility: hidden：按浏览器的规则，这次聚焦落空。 */
  hidden: boolean;
}

/**
 * 让 focus() 按浏览器的规则处理隐藏的元素：藏在 visibility: hidden 里的拿不到焦点。jsdom 照样给焦点，
 * 只看 document.activeElement 察觉不到聚焦得太早（浮层面板定位之前是隐藏的）。只看行内样式：jsdom 不做层叠，
 * 面板定位前的隐藏写在行内。返回每次调用的记录；用例结束时由 restoreMocks 还原。
 */
export function refuseHiddenFocus(): FocusCall[] {
  const calls: FocusCall[] = [];
  const focus = HTMLElement.prototype.focus;
  vi.spyOn(HTMLElement.prototype, "focus").mockImplementation(function (this: HTMLElement, options?: FocusOptions) {
    let hidden = false;
    for (let node: HTMLElement | null = this; node !== null && !hidden; node = node.parentElement) hidden = node.style.visibility === "hidden";
    calls.push({ target: this, hidden });
    if (!hidden) focus.call(this, options);
  });
  return calls;
}
