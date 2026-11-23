import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import mark from "../src/art/petal-mark.json";
import { buildInstallerHeader } from "./build-installer-header.mjs";

const APP_DIR = join(dirname(fileURLToPath(import.meta.url)), "..");
const TAURI_DIR = join(APP_DIR, "src-tauri");
const HEADER_FILE = "installer-header.bmp";
const TOKENS = readFileSync(join(APP_DIR, "src", "styles", "tokens.css"), "utf8");

const WIDTH = 150;
const HEIGHT = 57;
// 每行 450 字节补到 4 的倍数
const STRIDE = 452;

interface DecodedBmp {
  width: number;
  height: number;
  /** 自上而下、每像素「RRGGBB」。 */
  pixels: string[][];
}

// 解码器独立写一份，只认 NSIS 能稳妥加载的那一种：BITMAPINFOHEADER、24 位、不压缩、自下而上
function decodeBmp(bmp: Buffer): DecodedBmp {
  expect(bmp.subarray(0, 2).toString("latin1")).toBe("BM");
  expect(bmp.readUInt32LE(2)).toBe(bmp.length);
  expect(bmp.readUInt32LE(10)).toBe(54);
  expect(bmp.readUInt32LE(14)).toBe(40);
  const width = bmp.readInt32LE(18);
  const height = bmp.readInt32LE(22);
  expect(height).toBeGreaterThan(0);
  expect([bmp.readUInt16LE(26), bmp.readUInt16LE(28), bmp.readUInt32LE(30)]).toEqual([1, 24, 0]);
  const stride = Math.ceil((width * 3) / 4) * 4;
  expect(bmp.readUInt32LE(34)).toBe(stride * height);
  expect(bmp.length).toBe(54 + stride * height);
  const pixels: string[][] = [];
  for (let y = 0; y < height; y++) {
    const row = 54 + (height - 1 - y) * stride;
    const line: string[] = [];
    for (let x = 0; x < width; x++) {
      const at = row + x * 3;
      line.push(Buffer.from([bmp[at + 2], bmp[at + 1], bmp[at]]).toString("hex").toUpperCase());
    }
    expect([...bmp.subarray(row + width * 3, row + stride)].every((byte) => byte === 0)).toBe(true);
    pixels.push(line);
  }
  return { width, height, pixels };
}

/** 令牌在文件里第一次出现的值，即日樱 :root 块里的那个。 */
function tokenHex(css: string, name: string): string {
  const value = css.match(new RegExp(`${name}:\\s*#([0-9A-Fa-f]{6});`));
  if (value === null) throw new Error(`tokens.css 里没有 ${name}`);
  return value[1].toUpperCase();
}

/** 不看脚本，按构图直接算每个像素：花瓣 3 倍放在 (9, 4)，其余按列落在四级底色上。 */
function expectedPixels(grid: string[], css: string): string[][] {
  const bands: [number, string][] = [
    [69, tokenHex(css, "--sk-bg-3")],
    [93, tokenHex(css, "--sk-bg-2")],
    [111, tokenHex(css, "--sk-bg")],
    [WIDTH, "FFFFFF"],
  ];
  return Array.from({ length: HEIGHT }, (_, y) =>
    Array.from({ length: WIDTH }, (_, x) => {
      const cellX = Math.floor((x - 9) / 3);
      const cellY = Math.floor((y - 4) / 3);
      if (x >= 9 && y >= 4 && cellX < 16 && cellY < 16) {
        const cell = grid[cellY][cellX];
        if (cell !== ".") return (mark.palette as Record<string, string>)[cell].slice(1).toUpperCase();
      }
      const band = bands.find(([end]) => x < end);
      if (band === undefined) throw new Error(`第 ${x} 列不在任何底色条带里`);
      return band[1];
    }),
  );
}

function withCellFlipped(grid: string[], row: number, column: number, cell: string): string[] {
  return grid.map((line, y) => (y === row ? `${line.slice(0, column)}${cell}${line.slice(column + 1)}` : line));
}

const header = buildInstallerHeader(mark, TOKENS);

describe("NSIS 页眉图与母版、令牌同步", () => {
  it("重新生成的页眉图与仓库里的文件逐字节相同；改了母版或日樱底色要在 app/ 下跑 pnpm build:icons", () => {
    expect(header.equals(readFileSync(join(TAURI_DIR, HEADER_FILE)))).toBe(true);
  });

  it("tauri.conf.json 的 NSIS 页眉图指向这个文件，卸载向导沿用同一张", () => {
    const conf = JSON.parse(readFileSync(join(TAURI_DIR, "tauri.conf.json"), "utf8")) as { bundle: { windows: { nsis: Record<string, unknown> } } };
    expect(conf.bundle.windows.nsis.headerImage).toBe(HEADER_FILE);
    expect(conf.bundle.windows.nsis.uninstallerHeaderImage).toBeUndefined();
    expect(existsSync(join(TAURI_DIR, HEADER_FILE))).toBe(true);
  });

  it.each([
    ["母版一格", buildInstallerHeader({ ...mark, grid16: withCellFlipped(mark.grid16, 2, 9, "L") }, TOKENS)],
    ["日樱底色", buildInstallerHeader(mark, TOKENS.replace(/--sk-bg-3:\s*#[0-9A-Fa-f]{6};/, "--sk-bg-3: #EEE3E9;"))],
  ])("改动%s后页眉图必定与仓库文件不同，漂移逃不过上面的比对", (_, changed) => {
    expect(changed.equals(readFileSync(join(TAURI_DIR, HEADER_FILE)))).toBe(false);
  });
});

describe("页眉图的格式与像素", () => {
  const decoded = decodeBmp(header);

  it("是 150 x 57、24 位、不压缩、自下而上的 BMP，每行补齐到 452 字节", () => {
    expect([decoded.width, decoded.height]).toEqual([WIDTH, HEIGHT]);
    expect(header.length).toBe(54 + STRIDE * HEIGHT);
  });

  it("每个像素都等于构图算出的值：花瓣按母版 3 倍逐格画出，其余是从深到浅的日樱底色", () => {
    expect(decoded.pixels).toEqual(expectedPixels(mark.grid16, TOKENS));
  });

  it("花瓣描边落在预期位置，放大没有产生调色板之外的颜色", () => {
    // grid16 第 2 行第 9 列是描边 O，3 倍放大加偏移 (9, 4) 后落在 x 36-38、y 10-12
    for (let y = 10; y <= 12; y++) {
      for (let x = 36; x <= 38; x++) expect(decoded.pixels[y][x]).toBe("A9578A");
    }
    const seen = new Set(decoded.pixels.flat());
    const allowed = [...Object.values(mark.palette), "#FFFFFF", ...["--sk-bg-3", "--sk-bg-2", "--sk-bg"].map((name) => `#${tokenHex(TOKENS, name)}`)];
    expect([...seen].sort()).toEqual([...new Set(allowed.map((hex) => hex.slice(1).toUpperCase()))].sort());
  });

  it("最右一列是 NSIS 页眉的白底，控件右缘与页眉连成一片；最左一列是最深的 --sk-bg-3", () => {
    expect(decoded.pixels.map((line) => line[WIDTH - 1])).toEqual(Array(HEIGHT).fill("FFFFFF"));
    expect(decoded.pixels.map((line) => line[0])).toEqual(Array(HEIGHT).fill(tokenHex(TOKENS, "--sk-bg-3")));
  });

  it("花瓣实际占用的格子上下左右都留白，不贴边", () => {
    const outline = "A9578A";
    const rows = decoded.pixels.map((line, y) => (line.includes(outline) ? y : -1)).filter((y) => y >= 0);
    const columns = Array.from({ length: WIDTH }, (_, x) => x).filter((x) => decoded.pixels.some((line) => line[x] === outline));
    // 母版 16 格里第 2-13 行、第 2-14 列有内容
    expect([rows[0], rows[rows.length - 1]]).toEqual([10, 45]);
    expect([columns[0], columns[columns.length - 1]]).toEqual([15, 53]);
  });
});

describe("令牌读取", () => {
  it("缺少要用的底色令牌时报出令牌名", () => {
    expect(() => buildInstallerHeader(mark, TOKENS.replace(/--sk-bg-2:/, "--sk-bg-x:"))).toThrow("tokens.css 的 :root 块里没有 --sk-bg-2");
  });

  it("底色令牌不是 #RRGGBB 时报错，不生成错色", () => {
    expect(() => buildInstallerHeader(mark, TOKENS.replace(/--sk-bg:\s*#[0-9A-Fa-f]{6};/, "--sk-bg: color-mix(in srgb, red 10%, white);"))).toThrow(
      "--sk-bg 的值「color-mix(in srgb, red 10%, white)」不是 #RRGGBB",
    );
  });

  it("只认第一个 :root 块的日樱值，夜樱块里的同名令牌不影响页眉图", () => {
    const nightOnly = TOKENS.replace(/(:root\[data-sk-mode="night"\] \{)/, "$1\n  --sk-bg-3: #000000;");
    expect(buildInstallerHeader(mark, nightOnly).equals(header)).toBe(true);
  });
});
