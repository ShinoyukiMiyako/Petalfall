// @vitest-environment jsdom
import { useMemo } from "react";
import { describe, expect, it, vi } from "vitest";
import type { SpringName } from "../lib/motion";
import * as reflow from "../lib/reflow";
import { render } from "./dom";
import { spyUseReflow } from "./reflow-spy";

describe("useReflow 的替身", () => {
  it("挂上时记下容器与弹簧、停下时在真实现之后收尾；重渲染不换引用，sync 原样是真实现的那一个", () => {
    const log: string[] = [];
    const sync = vi.fn();
    // 真实现在 jsdom 里什么也不做，换成记账的一份，才看得出替身把挂上、停下与 sync 都交给了它
    const inner = <T extends HTMLElement>(spring: SpringName = "settle") =>
      useMemo(
        () =>
          Object.assign(
            (el: T | null) => {
              if (el === null) return;
              log.push(`inner 挂上 ${el.id} ${spring}`);
              return () => {
                log.push(`inner 停下 ${el.id}`);
              };
            },
            { sync },
          ),
        [spring],
      );
    const useReflow = spyUseReflow({ ...reflow, useReflow: inner }, (el, spring) => {
      log.push(`记下 ${el.id} ${spring}`);
      return () => {
        log.push(`收尾 ${el.id}`);
      };
    });
    const refs: ReturnType<typeof reflow.useReflow>[] = [];
    function Box({ label }: { label: string }) {
      const ref = useReflow<HTMLDivElement>("morph");
      refs.push(ref);
      return (
        <div id="box" ref={ref}>
          {label}
        </div>
      );
    }
    const view = render(<Box label="一" />);
    view.rerender(<Box label="二" />);
    expect(refs).toHaveLength(2);
    expect(refs[1]).toBe(refs[0]);
    expect(log).toEqual(["记下 box morph", "inner 挂上 box morph"]);
    expect(refs[0].sync).toBe(sync);
    refs[0].sync(1234);
    expect(sync).toHaveBeenCalledWith(1234);
    view.unmount();
    expect(log).toEqual(["记下 box morph", "inner 挂上 box morph", "inner 停下 box", "收尾 box"]);
  });
});
