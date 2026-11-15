// @vitest-environment jsdom
import { createHash } from "node:crypto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Pixels } from "../illustrations/canvas";
import { hslToRgb } from "../illustrations/color";
import { accountHue } from "../illustrations/seed";
import { stubCanvas } from "../illustrations/test-canvas";
import { FACE_RECT, HAT_RECT } from "./layout";
import { MANNEQUIN_SHADES, mannequinPixels, mannequinSkin, type MannequinCell } from "./mannequin";

const CLEAR = [0, 0, 0, 0];

const rgba = (pixels: Pixels, x: number, y: number) => {
  const i = (y * pixels.width + x) * 4;
  return [...pixels.data.slice(i, i + 4)];
};

const color = (hue: number, cell: MannequinCell) => [...hslToRgb(hue, MANNEQUIN_SHADES[cell][0], MANNEQUIN_SHADES[cell][1]), 255];

/** 一行像素按格子字母写出来，"." 为透明；比逐个断言好读。 */
function row(pixels: Pixels, hue: number, x: number, y: number, width: number): string {
  const cells = Object.keys(MANNEQUIN_SHADES) as MannequinCell[];
  let out = "";
  for (let col = 0; col < width; col++) {
    const at = rgba(pixels, x + col, y);
    if (at[3] === 0) {
      out += ".";
      continue;
    }
    const cell = cells.find((c) => color(hue, c).every((v, i) => v === at[i]));
    if (cell === undefined) throw new Error(`(${x + col}, ${y}) 的颜色 ${at.join(",")} 不在人偶调色板里`);
    out += cell;
  }
  return out;
}

const chroma = ([r, g, b]: readonly number[]) => Math.max(r, g, b) - Math.min(r, g, b);

describe("人偶皮肤的像素", () => {
  it("同一账户每次生成逐像素相同：色相只由 UUID 定，带不带连字符、大小写都一样", () => {
    const uuid = "6a1f0c0e-4b5d-4c3e-9f2a-7b8c9d0e1f20";
    const first = mannequinPixels(accountHue(uuid), "wide");
    const again = mannequinPixels(accountHue(uuid.replace(/-/g, "").toUpperCase()), "wide");
    expect([first.width, first.height]).toEqual([64, 64]);
    expect(again.data).toEqual(first.data);
    // 跨版本也不变：改了配色或排布，这里会提醒是有意为之
    expect(accountHue(uuid)).toBe(134);
    expect(createHash("sha256").update(first.data).digest("hex")).toBe("4b0b2f9107162a06fa048673b6a4fa0d9846d9bcfe3f04504d5ea74d91f5a754");
  });

  it("不同色相画出来不同：帽子与衣裤跟着色相走，人偶本体压到几乎是灰的", () => {
    const red = mannequinPixels(0, "wide");
    const teal = mannequinPixels(180, "wide");
    expect(teal.data).not.toEqual(red.data);
    // 帽顶、上衣正面、裤子正面
    for (const [x, y] of [[40, 0], [21, 22], [5, 22]] as const) {
      expect(rgba(teal, x, y)).not.toEqual(rgba(red, x, y));
      expect(chroma(rgba(red, x, y))).toBeGreaterThan(40);
    }
    for (let hue = 0; hue < 360; hue += 15) {
      const pixels = mannequinPixels(hue, "wide");
      // 额头与前臂是人偶本体
      expect(chroma(rgba(pixels, 9, 11))).toBeLessThanOrEqual(10);
      expect(chroma(rgba(pixels, 45, 30))).toBeLessThanOrEqual(10);
    }
  });

  it("正脸是光头人偶，两点眼在第 5 行第 3、6 列；帽子层上两行帽身、第三行帽檐，往下透明露出额头与眼睛", () => {
    const hue = 210;
    const pixels = mannequinPixels(hue, "wide");
    const face = Array.from({ length: 8 }, (_, y) => row(pixels, hue, FACE_RECT.x, FACE_RECT.y + y, 8));
    expect(face).toEqual(["MMMMMMMM", "MMMMMMMM", "MMMMMMMM", "MMMMMMMM", "MMEMMEMM", "MMMMMMMM", "MMMMMMMM", "MMMMMMMM"]);
    const hat = Array.from({ length: 8 }, (_, y) => row(pixels, hue, HAT_RECT.x, HAT_RECT.y + y, 8));
    expect(hat).toEqual(["KKKKKKKK", "KKKKKKKK", "RRRRRRRR", "........", "........", "........", "........", "........"]);
    // 帽顶整块是帽身，帽子底面透明
    expect(row(pixels, hue, 40, 0, 8)).toBe("KKKKKKKK");
    expect(row(pixels, hue, 48, 7, 8)).toBe("........");
    // 帽子的右侧面与背面同样三行
    expect([0, 1, 2, 3].map((y) => row(pixels, hue, 32, 8 + y, 8))).toEqual(["KKKKKKKK", "KKKKKKKK", "RRRRRRRR", "........"]);
    expect([0, 1, 2, 3].map((y) => row(pixels, hue, 56, 8 + y, 8))).toEqual(["KKKKKKKK", "KKKKKKKK", "RRRRRRRR", "........"]);
  });

  it("身体：领口露出脖子，上衣八行、一行下摆，再往下是裤腰；侧面与背面同样分段", () => {
    const hue = 30;
    const pixels = mannequinPixels(hue, "wide");
    const front = (y: number) => row(pixels, hue, 20, 20 + y, 8);
    expect([0, 1, 7, 8, 9, 11].map(front)).toEqual(["CCTMMTCC", "CCCCCCCC", "CCCCCCCC", "TTTTTTTT", "PPPPPPPP", "PPPPPPPP"]);
    expect([0, 8, 9].map((y) => row(pixels, hue, 32, 20 + y, 8))).toEqual(["CCCCCCCC", "TTTTTTTT", "PPPPPPPP"]);
    expect([0, 8, 9].map((y) => row(pixels, hue, 16, 20 + y, 4))).toEqual(["CCCC", "TTTT", "PPPP"]);
    // 肩顶是上衣，底面是裤子
    expect(row(pixels, hue, 20, 16, 8)).toBe("CCCCCCCC");
    expect(row(pixels, hue, 28, 19, 8)).toBe("PPPPPPPP");
  });

  it("双臂短袖加袖口、肘部一道关节缝，双腿裤子加两行鞋；左右两侧各自的贴图位置都画了", () => {
    const hue = 300;
    const pixels = mannequinPixels(hue, "wide");
    const arm = (x: number, y0: number) => Array.from({ length: 12 }, (_, y) => row(pixels, hue, x, y0 + y, 4)[0]).join("");
    // 右臂正面在 (44, 20)，左臂正面在 (36, 52)
    expect(arm(44, 20)).toBe("CCCTMMMJMMMM");
    expect(arm(36, 52)).toBe("CCCTMMMJMMMM");
    const leg = (x: number, y0: number) => Array.from({ length: 12 }, (_, y) => row(pixels, hue, x, y0 + y, 4)[0]).join("");
    expect(leg(4, 20)).toBe("PPPPPPPPPPSS");
    expect(leg(20, 52)).toBe("PPPPPPPPPPSS");
    // 手是人偶本体，鞋底是鞋
    expect(row(pixels, hue, 48, 16, 4)).toBe("MMMM");
    expect(row(pixels, hue, 8, 16, 4)).toBe("SSSS");
  });

  it("身体与四肢的外层、展开图之外的空位都是透明的", () => {
    const pixels = mannequinPixels(90, "wide");
    for (const [x, y] of [
      [20, 36], [44, 36], [52, 52], [4, 36], [4, 52], // 外层：身体、右臂、左臂、右腿、左腿的正面
      [0, 0], [60, 0], [56, 16], [60, 48], // 展开图之外
    ] as const) {
      expect(rgba(pixels, x, y)).toEqual(CLEAR);
    }
  });

  it("细体型手臂窄一列：贴图位置跟着变，宽体型背面最右一列在细体型里是空的", () => {
    const hue = 0;
    const wide = mannequinPixels(hue, "wide");
    const slim = mannequinPixels(hue, "slim");
    expect(slim.data).not.toEqual(wide.data);
    // 右臂背面：宽体型 (52, 20) 起 4 列，细体型 (51, 20) 起 3 列
    expect(row(wide, hue, 51, 20, 5)).toBe("CCCCC");
    expect(row(slim, hue, 51, 20, 5)).toBe("CCC..");
    // 左臂背面：宽体型 (44, 52) 起 4 列，细体型 (43, 52) 起 3 列
    expect(row(wide, hue, 43, 52, 5)).toBe("CCCCC");
    expect(row(slim, hue, 43, 52, 5)).toBe("CCC..");
  });
});

describe("mannequinSkin", () => {
  beforeEach(() => {
    stubCanvas();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("新格式、叠帽子层、体型照给，位图是 64 × 64；同一色相与体型给同一个对象，换色相或体型是另一张", () => {
    const skin = mannequinSkin(77, "slim");
    expect([skin.format, skin.hat, skin.model]).toEqual(["modern", true, "slim"]);
    expect([skin.bitmap.width, skin.bitmap.height]).toEqual([64, 64]);
    expect(mannequinSkin(77, "slim")).toBe(skin);
    expect(mannequinSkin(78, "slim")).not.toBe(skin);
    expect(mannequinSkin(77, "wide")).not.toBe(skin);
  });
});
