import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  APP_LICENSE,
  ASSET_LICENSE,
  ISSUES_URL,
  REPO_URL,
  buildInfo,
  loadAdditionalTerms,
  loadAssetLicense,
  loadLicenseText,
  loadPrivacyNotice,
  loadThirdPartyLicenses,
  loadUsageNotice,
  type ThirdPartyManifest,
} from "./app-info";
import { BundledFileError } from "./bundled-file";

const MANIFEST: ThirdPartyManifest = {
  packages: [
    { ecosystem: "npm", name: "react", version: "19.2.7", license: "MIT" },
    { ecosystem: "cargo", name: "serde", version: "1.0.228", license: "MIT OR Apache-2.0" },
  ],
};

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

/** 读失败的错误：message 是日志与启动错误页用的整句（带称呼），reason 是提示框标题下只放的原因。 */
async function rejection(load: () => Promise<unknown>): Promise<{ message: string; reason: string }> {
  const error: unknown = await load().then(
    () => {
      throw new Error("文件不在却读成功了");
    },
    (e: unknown) => e,
  );
  if (!(error instanceof BundledFileError)) throw new Error(`读失败的不是 BundledFileError：${String(error)}`);
  return { message: error.message, reason: error.reason };
}

/** 读失败时底下的原始错误（BundledFileError 的 cause）。 */
async function causeOf(load: () => Promise<unknown>): Promise<unknown> {
  const error: unknown = await load().then(
    () => {
      throw new Error("该失败的却读成功了");
    },
    (e: unknown) => e,
  );
  if (!(error instanceof BundledFileError)) throw new Error(`读失败的不是 BundledFileError：${String(error)}`);
  return error.cause;
}

/** fetch 拿不到响应时浏览器给的错误：只有一句话，不带地址。 */
function refusedFetch(): TypeError {
  const failure = new TypeError(`Failed to fetch ${Math.random().toString(36).slice(2)}`);
  vi.stubGlobal("fetch", async () => {
    throw failure;
  });
  return failure;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("仓库与协议", () => {
  it("问题反馈地址由仓库地址派生", () => {
    expect(REPO_URL).toMatch(/^https:\/\/github\.com\/ShinoyukiMiyako\/[A-Za-z0-9-]+$/);
    expect(ISSUES_URL).toBe(`${REPO_URL}/issues`);
  });

  it("开源协议与仓库根目录的 LICENSE 一致：AGPL 第 3 版，SPDX 写作 -only", () => {
    const license = readFileSync(join(REPO_ROOT, "LICENSE"), "utf8");
    expect(license).toMatch(/^\s*GNU AFFERO GENERAL PUBLIC LICENSE\s+Version 3,/);
    expect(APP_LICENSE).toBe("AGPL-3.0-only");
  });

  // 界面上说素材按这个许可授权，说法得和随产物分发的素材许可正文对得上
  it("素材许可与仓库根目录的素材许可正文一致：CC BY-NC-ND 4.0", () => {
    const assets = readFileSync(join(REPO_ROOT, "LICENSE-ASSETS.md"), "utf8");
    expect(ASSET_LICENSE).toBe("CC BY-NC-ND 4.0");
    expect(assets).toContain(ASSET_LICENSE);
    expect(assets).toContain("https://creativecommons.org/licenses/by-nc-nd/4.0/");
  });

  it("README 的许可一节只声明第 3 版，没有「或任何更高版本」的授权", () => {
    const readme = readFileSync(join(REPO_ROOT, "README.md"), "utf8");
    const section = readme.slice(readme.indexOf("## 许可"));
    expect(section).toContain("GNU Affero General Public License v3.0");
    expect(section).not.toMatch(/later|更高版本|or-later/i);
  });
});

describe("构建信息", () => {
  it("没经 vite 注入时报错，不给假值", () => {
    expect(() => buildInfo()).toThrow("构建信息没有注入");
  });

  it("注入后原样返回", () => {
    vi.stubGlobal("__PETALFALL_BUILD__", { version: "1.2.0-beta.2", channel: "beta", builtAt: "2026-10-31T08:15:30.000Z" });
    expect(buildInfo()).toEqual({ version: "1.2.0-beta.2", channel: "beta", builtAt: "2026-10-31T08:15:30.000Z" });
  });
});

describe("第三方许可清单", () => {
  it("从产物根目录读构建时写出的清单", async () => {
    const fetch = vi.fn(async () => new Response(JSON.stringify(MANIFEST), { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    await expect(loadThirdPartyLicenses()).resolves.toEqual(MANIFEST);
    expect(fetch.mock.calls).toEqual([["/third-party-licenses.json"]]);
  });

  it("文件不在（产物不是经 vite 构建出来的）时报错：整句带称呼、地址与状态码，原因单独一份不带称呼", async () => {
    vi.stubGlobal("fetch", async () => new Response("Not Found", { status: 404 }));
    expect(await rejection(loadThirdPartyLicenses)).toEqual({
      message: "第三方许可清单读不出来：/third-party-licenses.json 返回 404",
      reason: "/third-party-licenses.json 返回 404",
    });
  });

  it("请求本身失败（拿不到响应）时报错：整句带称呼与地址，原错误的说法接在后面，原错误留作 cause", async () => {
    const failure = refusedFetch();
    expect(await rejection(loadThirdPartyLicenses)).toEqual({
      message: `第三方许可清单读不出来：/third-party-licenses.json 请求失败：${failure.message}`,
      reason: `/third-party-licenses.json 请求失败：${failure.message}`,
    });
    expect(await causeOf(loadThirdPartyLicenses)).toBe(failure);
  });

  it("内容不是 JSON（例如开发服务器把缺的文件回落成页面）时报错：整句带称呼与地址，解析错误留作 cause", async () => {
    vi.stubGlobal("fetch", async () => new Response("<!doctype html><div id=\"root\"></div>", { status: 200 }));
    const cause = await causeOf(loadThirdPartyLicenses);
    if (!(cause instanceof SyntaxError)) throw new Error(`解析失败的原错误不是 SyntaxError：${String(cause)}`);
    expect(await rejection(loadThirdPartyLicenses)).toEqual({
      message: `第三方许可清单读不出来：/third-party-licenses.json 解析失败：${cause.message}`,
      reason: `/third-party-licenses.json 解析失败：${cause.message}`,
    });
  });
});

describe.each([
  { name: "协议全文", load: loadLicenseText, url: "/LICENSE.txt" },
  { name: "附加条款", load: loadAdditionalTerms, url: "/LICENSE-ADDITIONAL-TERMS.md" },
  { name: "素材许可", load: loadAssetLicense, url: "/LICENSE-ASSETS.md" },
])("$name", ({ name, load, url }) => {
  it("从产物根目录读构建时写出的文本，原样返回", async () => {
    // 随机一行：返回的必须是读回来的那份
    const text = `# ${name}\n\n  ${Math.random().toString(36).slice(2)}\n`;
    const fetch = vi.fn(async () => new Response(text, { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    await expect(load()).resolves.toBe(text);
    expect(fetch.mock.calls).toEqual([[url]]);
  });

  it("文件不在时报错：整句带称呼、地址与状态码，原因单独一份不带称呼", async () => {
    const status = 400 + Math.floor(Math.random() * 200);
    vi.stubGlobal("fetch", async () => new Response("Not Found", { status }));
    expect(await rejection(load)).toEqual({ message: `${name}读不出来：${url} 返回 ${status}`, reason: `${url} 返回 ${status}` });
  });

  it("请求本身失败时报错：整句带称呼与地址，原错误留作 cause", async () => {
    const failure = refusedFetch();
    expect(await rejection(load)).toEqual({ message: `${name}读不出来：${url} 请求失败：${failure.message}`, reason: `${url} 请求失败：${failure.message}` });
    expect(await causeOf(load)).toBe(failure);
  });
});

describe.each([
  { name: "使用须知", load: loadUsageNotice, url: "/usage-notice.md" },
  { name: "隐私说明", load: loadPrivacyNotice, url: "/privacy.md" },
])("$name", ({ name, load, url }) => {
  it("从产物根目录读构建时写出的文件，拆出文首的版本号与正文", async () => {
    const version = 1 + Math.floor(Math.random() * 999);
    const marker = Math.random().toString(36).slice(2);
    const fetch = vi.fn(async () => new Response(`# ${name}\r\n\r\n版本：${version}\r\n\r\n正文 ${marker}\r\n`, { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    await expect(load()).resolves.toEqual({ version, body: `正文 ${marker}` });
    expect(fetch.mock.calls).toEqual([[url]]);
  });

  it("文件不在时报错：整句带称呼、地址与状态码，原因单独一份不带称呼", async () => {
    const status = 400 + Math.floor(Math.random() * 200);
    vi.stubGlobal("fetch", async () => new Response("Not Found", { status }));
    expect(await rejection(load)).toEqual({ message: `${name}读不出来：${url} 返回 ${status}`, reason: `${url} 返回 ${status}` });
  });

  it("请求本身失败时报错：整句带称呼与地址，原错误留作 cause", async () => {
    const failure = refusedFetch();
    expect(await rejection(load)).toEqual({ message: `${name}读不出来：${url} 请求失败：${failure.message}`, reason: `${url} 请求失败：${failure.message}` });
    expect(await causeOf(load)).toBe(failure);
  });

  it("文首没有版本号时报错，不当成第 1 版：整句带称呼，原因单独一份不带称呼", async () => {
    vi.stubGlobal("fetch", async () => new Response(`# ${name}\n\n正文\n`, { status: 200 }));
    const reason = "文首应是一行「# 标题」，接着一行「版本：N」（N 是从 1 起的整数）";
    expect(await rejection(load)).toEqual({ message: `${name}读不出来：${reason}`, reason });
  });
});
