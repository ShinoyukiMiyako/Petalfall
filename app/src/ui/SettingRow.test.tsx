// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OverlayProvider, OverlayRoot } from "../shell/overlay";
import { render, type Rendered } from "../test/dom";
import { SettingRow } from "./SettingRow";
import styles from "./SettingRow.module.css";

// 让位挂到了哪些容器上：jsdom 不做布局，让位本身在这里测不了，只核对它挂对了地方；挂载照常交给真实现
const reflowed = vi.hoisted(() => new Set<Element>());
vi.mock("../lib/reflow", async (orig) => {
  const mod = await orig<typeof import("../lib/reflow")>();
  const { useCallback } = await import("react");
  return {
    ...mod,
    useReflow: <T extends HTMLElement>(spring: Parameters<typeof mod.useReflow>[0] = "settle") => {
      const real = mod.useReflow<T>(spring);
      return useCallback(
        (el: T | null) => {
          if (el !== null) reflowed.add(el);
          return real(el);
        },
        [real],
      );
    },
  };
});

let view: Rendered | undefined;
afterEach(() => {
  view?.unmount();
  view = undefined;
  reflowed.clear();
});

function mount(ui: ReactNode) {
  view = render(
    <OverlayProvider>
      {ui}
      <OverlayRoot />
    </OverlayProvider>,
  );
  return view.container;
}

describe("SettingRow", () => {
  it("把标题与说明的 id 交给控件，名称与可见标题一致", () => {
    const container = mount(
      <SettingRow title="自动下载 Java" description="缺少所需版本时自动下载">
        {({ titleId, descriptionId }) => <button type="button" aria-labelledby={titleId} aria-describedby={descriptionId} />}
      </SettingRow>,
    );
    const button = container.querySelector("button") as HTMLButtonElement;
    const title = document.getElementById(button.getAttribute("aria-labelledby") as string);
    const desc = document.getElementById(button.getAttribute("aria-describedby") as string);
    expect(title?.textContent).toBe("自动下载 Java");
    expect(desc?.textContent).toBe("缺少所需版本时自动下载");
  });

  it("没有说明时说明 id 为 undefined；折行标记只在开启时出现", () => {
    let seen: string | undefined = "unset";
    const container = mount(
      <SettingRow title="内存" stackOnCompact>
        {({ descriptionId }) => {
          seen = descriptionId;
          return <span>滑杆</span>;
        }}
      </SettingRow>,
    );
    expect(seen).toBeUndefined();
    expect((container.firstElementChild as HTMLElement).dataset.stack).toBe("true");
  });

  it("说明会变高的行打开 reflow：让位挂在行上（标题说明列与控件互相让位）；默认不挂", () => {
    const container = mount(
      <>
        <SettingRow title="游戏启动后" description="隐藏启动器" reflow>
          <span>分段</span>
        </SettingRow>
        <SettingRow title="自动下载 Java">
          <span>开关</span>
        </SettingRow>
      </>,
    );
    const [moving, still] = [...container.children];
    expect(reflowed.has(moving)).toBe(true);
    expect(reflowed.has(still)).toBe(false);
    // 让位项是行的两个直接子项：标题说明列与控件区
    expect([...moving.children].map((c) => c.textContent)).toEqual(["游戏启动后隐藏启动器", "分段"]);
  });
});

describe("设置行之间的分隔线", () => {
  /**
   * 样式表里画分隔线的那条规则（jsdom 不算样式，这里拿它的选择器去匹配 DOM）：
   * 局部类名换成 CSS 模块给的类名（vitest 按 stable 策略取名），声明块留着核对只画上沿。
   */
  const separator = (() => {
    const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "SettingRow.module.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    const rules = [...css.matchAll(/([^{}]+)\{([^}]*)\}/g)].filter((m) => /border-(top|bottom)\s*:/.test(m[2]));
    if (rules.length !== 1) throw new Error(`样式表里画分隔线的规则应正好一条，实际 ${rules.length} 条`);
    return { selector: rules[0][1].trim().replace(/\.row(?![\w-])/g, `.${styles.row}`), body: rules[0][2] };
  })();

  /**
   * 按 layout 摆一列兄弟：r 是设置行，x 是退场中的块（带 inert，按约定退场节点都带），b 是在场的别的块（提示条、下载块这类）。
   * 交回每一条设置行上沿有没有线。
   */
  function lines(layout: string): boolean[] {
    const container = mount(
      <div data-column="">
        {[...layout].map((kind, i) =>
          kind === "r" ? (
            <SettingRow key={i} title={`第 ${i} 项`}>
              <span>控件</span>
            </SettingRow>
          ) : (
            <div key={i} inert={kind === "x"}>
              块
            </div>
          ),
        )}
      </div>,
    );
    const column = container.querySelector("[data-column]");
    if (column === null) throw new Error("没有挂上这一列");
    const drawn = [...column.children].filter((el) => el.classList.contains(styles.row)).map((row) => row.matches(separator.selector));
    // 同一个用例里要摆好几列：每列看完就卸掉，不留在文档里
    view?.unmount();
    view = undefined;
    return drawn;
  }

  it("只画在行的上沿：首行之上不画，末行之下也不画", () => {
    expect(separator.body).toMatch(/border-top:\s*1px solid var\(--sk-line\);/);
    expect(separator.body).not.toMatch(/border-bottom/);
    expect(lines("rrr")).toEqual([false, true, true]);
  });

  it("两行之间夹着一块退场块：线从退场一开始就画在下一行上，不等它从文档里拿掉", () => {
    expect(lines("rxr")).toEqual([false, true]);
    expect(lines("rrxr")).toEqual([false, true, true]);
  });

  it("退场块在首行之前或末行之后：首行照旧不画，末行照旧", () => {
    expect(lines("xrr")).toEqual([false, true]);
    expect(lines("rrx")).toEqual([false, true]);
  });

  it("中间夹着在场的别的块：它两侧的行都不画线，退场块挨着它时也一样", () => {
    expect(lines("rbr")).toEqual([false, false]);
    expect(lines("rbxr")).toEqual([false, false]);
    expect(lines("rxbr")).toEqual([false, false]);
    expect(lines("brr")).toEqual([false, true]);
  });
});
