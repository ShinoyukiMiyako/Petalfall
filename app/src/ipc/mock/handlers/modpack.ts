// 整合包导入的假后端：实例名按后端同一套规则校验（写法、长度、保留名、不区分大小写的重名），
// 本地文件按扩展名认格式，平台整合包的版本与清单从共用的工程目录（ipc/mock/catalog）里取。导入是长任务：
// 登记任务、推送带 task_id 的事件、响应取消，成功后新实例进入实例列表，清单里的 Mod 落进它的 mods/。
// 文件名词干含 blocked 的 .zip 是带两个需要手动下载文件的 CurseForge 包：用户「下载」到下载文件夹里的文件
// 记在 mockDownloads（测试经 seedMockDownloads 放），认领过的记在 claimed，导入时只有都认领了才能成。
// 目录里标了 bodyBlocked 的平台版本，整合包文件本身也要手动下载：认领前摘要和导入都读不到清单。
// 这两份状态只在内存里，不进浏览器里的持久化快照。
// 设置里打开「自动下载受限的 CurseForge 文件」后，真后端在 download-url 端点拿不到地址时改用拼出的 CDN 地址，
// 拿到地址就照常下载、不再看认领过的副本；这里把两种受限夹具都当作探得到，预览不列受限文件、导入直接成功，
// 认领状态不影响结果。探不到（401、403、404、410）仍要手动下载的那一支不在夹具里。
// 导入成功时步骤表与后端同一规则；整合包自己那几步（拿包、读包、放文件、解覆盖文件、导入完成）推不推下载快照、报什么
// 阶段文字也照后端。与后端不同的有三处：装原版与加载器那几步后端还有各自的阶段文字（开始安装原版、安装器执行完成之类），
// 这里只报步骤与快照；后端先拿包、读包（报过这几步、平台整合包还下载过本体）再查跳过的可选文件与实例名、再占住名字，
// 本体受限没认领时也是进了拿包那一步才失败，这里在报任何步骤之前就查这些、占名字，跳过的不是可选文件、名字不合格、重名
// 与本体没认领时一条事件都不推；默认设置不真的套，只按配置报那一步，后端套上了文件时的那句阶段文字没有。
// 导出读 Mod mock 里的同一份 mods 目录与卷宗，额外项的文件数与体积用固定表（浏览器里没有磁盘）。

import type { InstallPhase } from "../../../domain/types";
import type { PlatformId } from "../../discover";
import { IpcError } from "../../errors";
import { checkInstanceName, type InstanceDto, type NameRule } from "../../instances";
import type {
  BlockedFileDto,
  ClaimReportDto,
  ExportBundledModDto,
  ExportExtra,
  ExportExtraStatDto,
  ExportIndexedModDto,
  ModpackExportOptions,
  ModpackExportOutcomeDto,
  ModpackExportPreviewDto,
  ModpackInstallOutcomeDto,
  ModpackPreviewDto,
  ModpackSource,
  ModpackSummaryDto,
  PackFileAvailability,
  PackFormat,
  PackLoader,
  PackLoaderKind,
  PackModDto,
} from "../../modpack";
import { DISTRIBUTION_BLOCKED, EXPORT_EXTRAS, parseBlockedFiles } from "../../modpack";
import { findMockSource, type MockPackManifest } from "../catalog";
import type { MockArgs, MockHandlers } from "../types";
import { ensureMockGameDir, MOCK_DATA_DIR, mockDownloadRestrictedCurseforgeFiles, mockNewInstanceTemplate, tauriArgError } from "./config";
import { MOCK_GAME_DIR, argReader, emitCoreEvent, parsePlatform, runMockTask } from "./discover";
import { NAME_MESSAGES, acquireMockInstance, acquireMockInstanceWithBases, addMockInstance, ensureMockSavesSettledForReading, handlers as instanceHandlers, markMockSavesRead, recordMockExport, mockInstanceIds, mockInstanceWorlds, MOCK_INSTALL_STEPS, MOCK_STEP_MS, phase, type MockInstanceHold } from "./instances";
import { DISABLED_SUFFIX, ledgerKey, mockInstance, mockModsDir, placeMockPackMods } from "./mods";
import { isMockTaskCancelled } from "./tasks";

/** 本地整合包文件的假清单：读不到真文件，每个本地包都报这一份。 */
const LOCAL_MANIFEST: MockPackManifest = {
  remoteFiles: 48,
  modFiles: 41,
  remoteBytes: 96_468_992,
  overrideFiles: 57,
  overrideBytes: 364_233,
  loader: { kind: "fabric", version: "0.16.9" },
};
const LOCAL_MINECRAFT = "1.21.1";
const LOCAL_VERSION = "1.0.0";

/** 系统下载文件夹：downloads_folder 给它，claim_blocked_files 只在它里面找得到 mockDownloads 里的文件。 */
export const MOCK_DOWNLOADS_DIR = "C:\\Users\\Player\\Downloads";

/** 带受限文件的 CurseForge 夹具里那两个需要手动下载的文件。 */
const BLOCKED_FIXTURE: readonly BlockedFileDto[] = [
  {
    project_id: 900001,
    file_id: 5000001,
    file_name: "blocked-a.jar",
    web_url: "https://www.curseforge.com/minecraft/mc-mods/blocked-a/files/5000001",
    destination: "mods/blocked-a.jar",
    sha1: "3f786850e387550fdab836ed7e6dc881de23001b",
    fingerprint: 0,
    size: 1024,
  },
  {
    project_id: 900002,
    file_id: 5000002,
    file_name: "blocked-b.jar",
    web_url: "https://www.curseforge.com/minecraft/mc-mods/blocked-b/files/5000002",
    destination: "mods/blocked-b.jar",
    sha1: "89e6c98d92887913cadf06b2adb97f26cde4849b",
    fingerprint: 0,
    size: 1024,
  },
];

/** 本地 .mrpack 夹具里的可选文件：一个落进 mods/，一个是资源包。 */
const OPTIONAL_MOD = "mods/optional-helper.jar";
const OPTIONAL_EXTRA = "resourcepacks/optional-pack.zip";
/** 覆盖层 mods/ 里随包自带的 jar，三种本地夹具都有。 */
const BUNDLED_MODS = ["mods/config-helper.jar", "mods/pack-tweaks.jar"];

/** 下载文件夹里已有的文件（小写文件名）。 */
const mockDownloads = new Set<string>();
/** 已认领、暂存位里有校验通过副本的文件 id。 */
const claimed = new Set<number>();

/** 测试辅助：下载文件夹里现在有这些文件（整份换掉）。 */
export function seedMockDownloads(names: readonly string[]): void {
  mockDownloads.clear();
  for (const name of names) mockDownloads.add(name.toLowerCase());
}

export function reset(): void {
  mockDownloads.clear();
  claimed.clear();
}

/** 与后端 validate_instance_name 一致：去掉首尾空白后按实例名规则校验（不查重）；返回这个名字或不合格的原因。 */
function checkName(raw: string): { name: string } | { rule: NameRule } {
  const name = raw.trim();
  const rule = checkInstanceName(name, []);
  return rule === null ? { name } : { rule };
}

function nameError(name: string, rule: NameRule): IpcError {
  return new IpcError("instance_name_invalid", `实例名 ${name} 不可用：${NAME_MESSAGES[rule]}`, { name, rule });
}

function formatOf(path: string): PackFormat {
  const lower = path.toLowerCase();
  if (lower.endsWith(".mrpack")) return "modrinth";
  if (lower.endsWith(".zip")) return "curseforge";
  throw new IpcError("modpack_unsupported_extension", `只认 .mrpack 和 CurseForge 导出的 .zip：${path}`, { path });
}

function fileOf(path: string): string {
  const file = path.split(/[\\/]/).pop();
  if (file === undefined) throw new Error("split 至少返回一段");
  return file;
}

function stemOf(path: string): string {
  const file = fileOf(path);
  const dot = file.lastIndexOf(".");
  return dot > 0 ? file.slice(0, dot) : file;
}

/** 本地夹具的种类：按扩展名认格式，词干含 blocked 的 .zip 带受限文件。 */
type LocalFixture = "mrpack" | "zip" | "blocked";

function fixtureOf(path: string): LocalFixture {
  if (formatOf(path) === "modrinth") return "mrpack";
  return stemOf(path).toLowerCase().includes("blocked") ? "blocked" : "zip";
}

function packEntry(path: string, ids: { project_id: number; file_id: number } | null, optional: boolean): PackModDto {
  const file = path.split("/").pop();
  if (file === undefined) throw new Error("split 至少返回一段");
  return {
    path,
    file_name: file,
    origin: "remote",
    optional,
    project_id: ids === null ? null : ids.project_id,
    file_id: ids === null ? null : ids.file_id,
    availability: "download",
  };
}

/** 夹具的 Mod 清单：条数与 LOCAL_MANIFEST.modFiles 一致，按落点小写排序（与后端同一口径）。 */
function fixtureMods(fixture: LocalFixture): PackModDto[] {
  const bundled: PackModDto[] = BUNDLED_MODS.map((path) => ({ ...packEntry(path, null, false), origin: "override", availability: "bundled" }));
  let special: PackModDto[] = [];
  if (fixture === "mrpack") special = [packEntry(OPTIONAL_MOD, null, true)];
  if (fixture === "blocked") {
    special = BLOCKED_FIXTURE.map((f) => ({ ...packEntry(f.destination, f, false), availability: restrictedAvailability(f.file_id) }));
  }
  const plain = LOCAL_MANIFEST.modFiles - bundled.length - special.length;
  const remote = Array.from({ length: plain }, (_, i) => {
    const n = String(i + 1).padStart(2, "0");
    return packEntry(`mods/pack-mod-${n}.jar`, fixture === "mrpack" ? null : { project_id: 800_000 + i, file_id: 4_000_000 + i }, false);
  });
  return [...remote, ...special, ...bundled].sort((a, b) => (a.path.toLowerCase() < b.path.toLowerCase() ? -1 : 1));
}

/** 夹具里能在导入前取消勾选的文件落点。 */
function fixtureOptional(fixture: LocalFixture): readonly string[] {
  return fixture === "mrpack" ? [OPTIONAL_MOD, OPTIONAL_EXTRA] : [];
}

/** 受限夹具里一个文件的大小：夹具都写了，没写是夹具本身的错。 */
function fixtureSize(file: BlockedFileDto): number {
  if (file.size === null) throw new Error(`mock 受限夹具 ${file.file_name} 缺少 size`);
  return file.size;
}

function unclaimedBlocked(): BlockedFileDto[] {
  return BLOCKED_FIXTURE.filter((f) => !claimed.has(f.file_id)).map((f) => ({ ...f }));
}

/** 受限夹具里一个文件的到手方式：开关打开时拼出的地址优先，认领过的副本用不上。 */
function restrictedAvailability(fileId: number): PackFileAvailability {
  if (mockDownloadRestrictedCurseforgeFiles()) return "download";
  return claimed.has(fileId) ? "claimed" : "blocked";
}

/** 命令自己校验的空白值报 invalid_argument（与后端 non_blank 一致）。 */
function nonBlank(field: string, value: string): string {
  const trimmed = value.trim();
  if (trimmed === "") throw new IpcError("invalid_argument", `${field} 不能为空`);
  return trimmed;
}

function parseSource(raw: unknown): ModpackSource {
  // source 的形状由 serde 反序列化：认不出来是 Tauri 的参数错误，到前端归一为 unknown。
  const malformed = () => new IpcError("unknown", `invalid args \`source\` for command \`install_modpack\`: ${JSON.stringify(raw)}`);
  if (typeof raw !== "object" || raw === null) throw malformed();
  const source = raw as Record<string, unknown>;
  const text = (key: string) => {
    const value = source[key];
    if (typeof value !== "string") throw malformed();
    return value;
  };
  if (source.kind === "platform") {
    return {
      kind: "platform",
      platform: parsePlatform(text("platform")),
      project_id: nonBlank("project_id", text("project_id")),
      version_id: nonBlank("version_id", text("version_id")),
    };
  }
  if (source.kind === "file") return { kind: "file", path: nonBlank("path", text("path")) };
  throw malformed();
}

/** 一个整合包导入要用的全部事实：名字、格式、来源、版本、清单。 */
interface PackFacts {
  name: string;
  format: PackFormat;
  platform: PlatformId | null;
  version: string;
  minecraft: string;
  /** 卷宗外的 Mod 文件名前缀。 */
  slug: string;
  /** 整合包本体的文件名：平台整合包是目录里这个版本的文件，本地包是选的那个文件。 */
  packFile: string;
  packBytes: number;
  manifest: MockPackManifest;
  /** 整合包文件本身受限时的那一项，其余 null。 */
  blockedBody: BlockedFileDto | null;
}

function packOf(source: ModpackSource): PackFacts {
  if (source.kind === "file") {
    return {
      name: stemOf(source.path),
      format: formatOf(source.path),
      platform: null,
      version: LOCAL_VERSION,
      minecraft: LOCAL_MINECRAFT,
      slug: "pack",
      packFile: fileOf(source.path),
      packBytes: 0,
      manifest: LOCAL_MANIFEST,
      blockedBody: null,
    };
  }
  const notFound = () =>
    new IpcError("mod_version_not_found", `平台 ${source.platform} 上工程 ${source.project_id} 未找到版本 ${source.version_id}`, {
      platform: source.platform,
      project_id: source.project_id,
      version_id: source.version_id,
    });
  const found = findMockSource(source.platform, source.project_id);
  if (found === undefined || found.project.resourceType !== "modpack") throw notFound();
  const version = found.source.versions.find((v) => v.info.version_id === source.version_id);
  if (version === undefined) throw notFound();
  if (version.manifest === null) throw new Error(`mock 目录里整合包 ${found.project.title} 的版本 ${source.version_id} 缺少清单`);
  const minecraft = version.info.game_versions[0];
  if (minecraft === undefined) throw new Error(`mock 目录里整合包 ${found.project.title} 的版本 ${source.version_id} 没有游戏版本`);
  if (version.info.file_size === null) throw new Error(`mock 目录里整合包 ${found.project.title} 的版本 ${source.version_id} 缺少 file_size`);
  return {
    name: found.project.title,
    format: source.platform === "modrinth" ? "modrinth" : "curseforge",
    platform: source.platform,
    version: version.info.version_number,
    minecraft,
    slug: found.source.slug,
    packFile: version.info.file_name,
    packBytes: version.info.file_size,
    manifest: version.manifest,
    blockedBody: version.bodyBlocked
      ? {
          project_id: Number(source.project_id),
          file_id: Number(source.version_id),
          file_name: version.info.file_name,
          web_url: `https://www.curseforge.com/minecraft/modpacks/${found.source.slug}/files/${source.version_id}`,
          destination: version.info.file_name,
          sha1: null,
          fingerprint: 2_718_281_828,
          size: version.info.file_size,
        }
      : null,
  };
}

/** 与后端一样，读包之前就要拿到文件：没认领过的受限本体报只有这个包的清单；开关打开时本体同样从拼出的地址下载。 */
function assertBodyClaimed(pack: PackFacts): void {
  const body = pack.blockedBody;
  if (body === null || claimed.has(body.file_id) || mockDownloadRestrictedCurseforgeFiles()) return;
  throw new IpcError(DISTRIBUTION_BLOCKED, `这些文件的作者不允许启动器直接下载，需要到 CurseForge 网页手动下载：${body.file_name}`, { files: [body] });
}

function previewModpack(args: MockArgs): ModpackPreviewDto {
  const path = nonBlank("path", argReader("preview_modpack", args).string("path"));
  const format = formatOf(path);
  const fixture = fixtureOf(path);
  return {
    format,
    name: stemOf(path),
    version: LOCAL_VERSION,
    summary: format === "modrinth" ? "本地整合包" : null,
    author: format === "curseforge" ? "未知作者" : null,
    minecraft: LOCAL_MINECRAFT,
    loader: LOCAL_MANIFEST.loader,
    remote_files: LOCAL_MANIFEST.remoteFiles,
    mod_files: LOCAL_MANIFEST.modFiles,
    remote_bytes: LOCAL_MANIFEST.remoteBytes,
    override_files: LOCAL_MANIFEST.overrideFiles,
    override_bytes: LOCAL_MANIFEST.overrideBytes,
    mods: fixtureMods(fixture),
    optional_extras: fixture === "mrpack" ? [packEntry(OPTIONAL_EXTRA, null, true)] : [],
    blocked_files: fixture === "blocked" && !mockDownloadRestrictedCurseforgeFiles() ? unclaimedBlocked() : [],
  };
}

/** Option<Vec<String>>：缺省与 null 都是不跳过任何文件；每一项去掉首尾空白后不能为空（与后端登记任务前的校验一致）。 */
function parseExcluded(args: MockArgs): string[] {
  if (args.excludedOptional === undefined || args.excludedOptional === null) return [];
  const paths = argReader("install_modpack", args).strings("excludedOptional");
  if (paths.some((p) => p.trim() === "")) throw new IpcError("invalid_argument", "excludedOptional 里有空路径");
  return paths;
}

/** 跳过的必须是这个包里的可选文件，比较不分大小写（Windows 下落点不分大小写）。 */
function checkExcluded(excluded: readonly string[], optional: readonly string[]): void {
  const known = new Set(optional.map((p) => p.toLowerCase()));
  const stray = excluded.find((p) => !known.has(p.toLowerCase()));
  if (stray !== undefined) throw new IpcError("invalid_argument", `不能跳过 ${stray}：它不是这个整合包里的可选文件`, { path: stray });
}

function loaderDto(loader: PackLoader | null) {
  return loader === null ? null : { kind: loader.kind, version: loader.version };
}

// ---------------------------------------------------------------- 导入与导出共用的节奏

/**
 * 假导入与假导出的节奏。默认的快节奏给 vitest：一次导入要走三四十拍，几十次导入、导出大多跑在真实计时器上，而 Windows 上的
 * 计时器哪怕设 0 ms 也要等约 15 ms，所以快节奏每拍只让出一次事件循环、不经计时器。浏览器预览由 mock 的入口换成与假安装
 * 同一拍（MOCK_STEP_MS）：导入里报下载的一步走 1.5 秒、整次十几秒，导出整次 3 秒，总进度与阶段进度两条都看得清。
 */
export type MockModpackPace = "fast" | "preview";
/** 每一拍的毫秒数；0 是只让出一次事件循环。 */
const PACE_STEP_MS: Readonly<Record<MockModpackPace, number>> = { fast: 0, preview: MOCK_STEP_MS };
/** 一批假下载推的快照条数，与假安装一致。 */
const BATCH_SNAPSHOTS = MOCK_INSTALL_STEPS;
/** 不报下载的一步停的拍数：预览里看得见它，又比报下载的步骤短。 */
const IDLE_STEPS = 2;
let paceStepMs = PACE_STEP_MS.fast;

/** 换假导入与假导出的节奏。不随 reset 复位：它与命令延迟一样是看界面用的开关，不是假数据。 */
export function setMockModpackPace(pace: MockModpackPace): void {
  paceStepMs = PACE_STEP_MS[pace];
}

/**
 * 等一拍，等完查取消。快节奏在 Node 里用 setImmediate：开着假计时器的用例照样要推时钟才走，假时钟上每拍约 1 ms，
 * 一次导入或导出都不超过原来的 4 拍 x 15 ms；浏览器里没有 setImmediate，退回 0 ms 的计时器。
 */
async function paceTick(taskId: string): Promise<void> {
  await new Promise<void>((resolve) => {
    if (paceStepMs > 0) setTimeout(resolve, paceStepMs);
    else if (typeof setImmediate === "function") setImmediate(resolve);
    else setTimeout(resolve, 0);
  });
  cancelledCheck(taskId);
}

/** 不报进度的一步：停 IDLE_STEPS 拍，期间被取消即以 cancelled 拒绝。 */
async function idleStep(taskId: string): Promise<void> {
  for (let tick = 0; tick < IDLE_STEPS; tick++) await paceTick(taskId);
}

// ---------------------------------------------------------------- 导入的步骤

interface MockBatch {
  files: number;
  bytes: number;
}

/**
 * 一批假下载：BATCH_SNAPSHOTS 条快照，每条之前等一拍。字节按拍匀速走，单个文件的一批也看得出在走；文件数跟着取整，
 * 最后一条才全部下完（前端不认一出现就下完的批次）。速度按预览的节奏算，测试不看它。
 */
async function fakeBatch(taskId: string, batch: MockBatch): Promise<void> {
  const speed = Math.round(batch.bytes / ((BATCH_SNAPSHOTS * MOCK_STEP_MS) / 1000));
  for (let step = 1; step <= BATCH_SNAPSHOTS; step++) {
    await paceTick(taskId);
    const bytes = Math.round((batch.bytes * step) / BATCH_SNAPSHOTS);
    const finished = step === BATCH_SNAPSHOTS ? batch.files : Math.floor((batch.files * step) / BATCH_SNAPSHOTS);
    emitCoreEvent({ task_id: taskId, kind: "download", total: batch.files, finished, bytes, total_bytes: batch.bytes, transferred: bytes, speed });
  }
}

/** 装原版与加载器那几步。 */
type RuntimeStep = "metadata" | "libraries" | "assets" | "natives" | "loader_profile" | "loader_queue" | "java" | "loader_installer" | "loader_libraries" | "loader_processors";

/**
 * 装原版与加载器各步的假下载，null 是这一步不报下载。同一次导入里任意两批的文件数与总字节不会都相同：前端按这两项认批次。
 * Java 按本机已有合适的版本走，不下载。
 */
const RUNTIME_BATCHES: Readonly<Record<RuntimeStep, MockBatch | null>> = {
  metadata: { files: 2, bytes: 196_608 },
  libraries: { files: 40, bytes: 20_971_520 },
  assets: { files: 1_200, bytes: 24_576_000 },
  natives: null,
  loader_profile: null,
  // 假后端里没有别的 Forge 安装在跑，不用等
  loader_queue: null,
  java: null,
  loader_installer: { files: 1, bytes: 6_291_456 },
  loader_libraries: { files: 24, bytes: 7_864_320 },
  loader_processors: null,
};

function isRuntimeStep(key: InstallPhase): key is RuntimeStep {
  return key in RUNTIME_BATCHES;
}

/** 拿到包与读包：平台整合包先下载本体。读完包也改不了这两步，预排的表与确切的表都以它们开头。 */
function packSteps(source: ModpackSource): InstallPhase[] {
  return source.kind === "platform" ? ["modpack_download", "modpack_read"] : ["modpack_read"];
}

/** 与后端 import_prelude_plan 一致：读到包之前只知道来源，加载器、有没有远程文件与覆盖文件都不知道，按没有加载器、两样都有预排。 */
function preludePlan(source: ModpackSource): InstallPhase[] {
  return [...packSteps(source), "metadata", "libraries", "assets", "natives", "modpack_files", "modpack_overrides"];
}

/** 与后端 install_reporting 同一顺序：Fabric、Quilt 先取 profile 再下库；Forge、NeoForge 先排队、再找 Java，再下安装器、下库、跑处理器。 */
function loaderSteps(loader: PackLoader | null): InstallPhase[] {
  if (loader === null) return [];
  switch (loader.kind) {
    case "fabric":
    case "quilt":
      return ["loader_profile", "loader_libraries"];
    case "forge":
    case "neoforge":
      return ["loader_queue", "java", "loader_installer", "loader_libraries", "loader_processors"];
  }
}

/** 读完包之后的确切步骤表，与后端同一规则；claimedFiles 是要放入的手动下载的文件数。 */
function exactPlan(source: ModpackSource, manifest: MockPackManifest, claimedFiles: number): InstallPhase[] {
  const plan: InstallPhase[] = [...packSteps(source), "metadata", "libraries", "assets", "natives", ...loaderSteps(manifest.loader)];
  if (claimedFiles > 0 || manifest.remoteFiles > 0) plan.push("modpack_files");
  if (manifest.overrideFiles > 0) plan.push("modpack_overrides");
  if (mockNewInstanceTemplate()) plan.push("template");
  return plan;
}

/** 进入一步：有假下载的推一批快照，没有的停 IDLE_STEPS 拍；期间被取消即以 cancelled 拒绝。 */
async function walk(taskId: string, plan: readonly InstallPhase[], key: InstallPhase, batch: MockBatch | null): Promise<void> {
  phase(taskId, plan, key);
  if (batch !== null) {
    await fakeBatch(taskId, batch);
    return;
  }
  await idleStep(taskId);
}

function installModpack(args: MockArgs): Promise<ModpackInstallOutcomeDto> {
  const read = argReader("install_modpack", args);
  const taskId = read.string("taskId");
  const source = parseSource(args.source);
  const requested = read.nullableString("name");
  const excluded = parseExcluded(args);
  ensureMockGameDir(args, "install_modpack");
  // 与后端一样，占用放在导入主体之外，导入无论怎样结束都在最后放开
  let hold: MockInstanceHold | null = null;
  return runMockTask(taskId, async () => {
    const pack = packOf(source);
    assertBodyClaimed(pack);
    const fixture = source.kind === "file" ? fixtureOf(source.path) : null;
    // 平台整合包的目录夹具没有可选文件：跳过任何文件都不对
    checkExcluded(excluded, fixture === null ? [] : fixtureOptional(fixture));
    const raw = requested === null ? pack.name : requested;
    const checked = checkName(raw);
    if ("rule" in checked) throw nameError(raw, checked.rule);
    const name = checked.name;
    const lower = name.toLowerCase();
    // 已占用的名字以实例 mock 为准，导入成功的实例也记进那里；整合包要的原版自然目录也占着它的版本号。
    if (lower === pack.minecraft.toLowerCase() || mockInstanceIds().some((v) => v.toLowerCase() === lower)) throw nameError(name, "duplicate");
    // 实例名一定下来就以「安装」占住，连同将要继承的原版一起占到导入结束：这期间启动、改名、删除这个实例，
    // 以及删除、改名、补全那个原版都报 instance_busy
    hold = acquireMockInstanceWithBases(name, "installing", [pack.minecraft]);

    const { manifest } = pack;
    // 与后端一样，读到包之前先按来源预排一份步骤表，读完再把还没走到的步骤换成确切的
    const prelude = preludePlan(source);
    if (source.kind === "platform") {
      // 与后端 download_platform_pack 一样先报一句再下载；本体受限、关着自动下载时用的是认领到的副本（没认领的上面已经拒了），
      // 只报这一句、不推下载快照
      phase(taskId, prelude, "modpack_download");
      const fromClaim = pack.blockedBody !== null && !mockDownloadRestrictedCurseforgeFiles();
      emitCoreEvent({ task_id: taskId, kind: "stage", message: fromClaim ? `使用手动下载的整合包 ${pack.packFile}` : `下载整合包 ${pack.packFile}` });
      if (fromClaim) await idleStep(taskId);
      else await fakeBatch(taskId, { files: 1, bytes: pack.packBytes });
    }
    await walk(taskId, prelude, "modpack_read", null);
    emitCoreEvent({ task_id: taskId, kind: "stage", message: `读取整合包 ${pack.name}：${manifest.remoteFiles} 个远程文件，${manifest.overrideFiles} 个覆盖文件` });
    // 开关打开时受限文件与其余远程文件一起下载，没有放入手动下载文件这一步
    let claimedFiles = 0;
    let claimedBytes = 0;
    if (fixture === "blocked" && !mockDownloadRestrictedCurseforgeFiles()) {
      // 与后端一样在读包时判定：还有没认领到的受限文件就整体失败，不建实例
      const missing = unclaimedBlocked();
      if (missing.length > 0) {
        const names = missing.map((f) => f.file_name).join("、");
        throw new IpcError(DISTRIBUTION_BLOCKED, `这些文件的作者不允许启动器直接下载，需要到 CurseForge 网页手动下载：${names}`, { files: missing });
      }
      claimedFiles = BLOCKED_FIXTURE.length;
      claimedBytes = BLOCKED_FIXTURE.reduce((sum, f) => sum + fixtureSize(f), 0);
    }
    const plan = exactPlan(source, manifest, claimedFiles);
    for (const key of plan) if (isRuntimeStep(key)) await walk(taskId, plan, key, RUNTIME_BATCHES[key]);
    if (plan.includes("modpack_files")) {
      phase(taskId, plan, "modpack_files");
      if (claimedFiles > 0) emitCoreEvent({ task_id: taskId, kind: "stage", message: `放入 ${claimedFiles} 个手动下载的文件` });
      // 认领到的文件从暂存位复制，与后端一样不算进下载的那一批
      const downloads = manifest.remoteFiles - claimedFiles;
      if (downloads > 0) {
        emitCoreEvent({ task_id: taskId, kind: "stage", message: `下载 ${downloads} 个整合包文件` });
        await fakeBatch(taskId, { files: downloads, bytes: manifest.remoteBytes - claimedBytes });
      }
    }
    if (plan.includes("modpack_overrides")) {
      phase(taskId, plan, "modpack_overrides");
      emitCoreEvent({ task_id: taskId, kind: "stage", message: `解出 ${manifest.overrideFiles} 个覆盖文件` });
      await idleStep(taskId);
    }
    // mock 不真的套默认设置，只按配置报这一步，步骤表与后端一致
    if (plan.includes("template")) await walk(taskId, plan, "template", null);
    const modpack = {
      platform: pack.platform,
      project_id: source.kind === "platform" ? source.project_id : null,
      version_id: source.kind === "platform" ? source.version_id : null,
      name: pack.name,
      version: pack.version,
    };
    addMockInstance(name, pack.minecraft, loaderDto(manifest.loader), modpack);
    placeMockPackMods(name, pack.minecraft, manifest.loader === null ? [] : [manifest.loader.kind], manifest.modFiles, pack.slug);
    emitCoreEvent({ task_id: taskId, kind: "stage", message: `整合包已导入为实例 ${name}` });
    return {
      version_id: name,
      version_dir: `${MOCK_GAME_DIR}/versions/${name}`,
      modpack: {
        schema: 1,
        format: pack.format,
        ...modpack,
        minecraft: pack.minecraft,
        loader: loaderDto(manifest.loader),
        remote_files: manifest.remoteFiles,
        override_files: manifest.overrideFiles,
        installed_at: Math.floor(Date.now() / 1000),
      },
    };
  }).finally(() => {
    if (hold !== null) hold.release();
  });
}

/** 与后端 is_safe_file_name 一致：单个文件名，不含路径分隔符、冒号与控制字符，不以空格或点结尾。 */
function isSafeFileName(name: string): boolean {
  return name !== "" && name !== "." && name !== ".." && !/[ .]$/.test(name) && !/[\\/:\p{Cc}]/u.test(name);
}

function claimStagingPath(file: BlockedFileDto): string {
  return `${MOCK_DATA_DIR}\\cache\\modpacks\\claims\\curseforge-${file.file_id}\\${file.file_name}`;
}

/**
 * 认领手动下载的文件。暂存位里已经认领过的直接算找到（与后端一样，不看这次选的是哪个文件夹）；
 * 其余只在选的是下载文件夹（不分大小写）、且 mockDownloads 里有同名文件时找到。
 */
function claimBlockedFiles(args: MockArgs): ClaimReportDto {
  const cmd = "claim_blocked_files";
  // 与后端 non_blank 一致：去掉首尾空白后再用，选文件夹时带进来的空白不让下载文件夹认不出来
  const folder = argReader(cmd, args).string("folder").trim();
  let files: BlockedFileDto[];
  try {
    files = parseBlockedFiles(args.files, "files");
  } catch (error) {
    // 形状由 serde 反序列化：不对是 Tauri 的参数错误
    throw tauriArgError(cmd, "files", error instanceof Error ? error.message : String(error));
  }
  if (folder === "") throw new IpcError("invalid_argument", "folder 不能为空");
  if (files.length === 0) throw new IpcError("invalid_argument", "files 不能为空");
  const seen = new Set<number>();
  for (const file of files) {
    if (!isSafeFileName(file.file_name)) throw new IpcError("invalid_argument", `file_name 不是单个文件名：${file.file_name}`);
    if (seen.has(file.file_id)) throw new IpcError("invalid_argument", `file_id 重复：${file.file_id}`);
    seen.add(file.file_id);
  }
  const inDownloads = folder.toLowerCase() === MOCK_DOWNLOADS_DIR.toLowerCase();
  const report: ClaimReportDto = { claimed: [], missing: [] };
  for (const file of files) {
    if (claimed.has(file.file_id)) {
      report.claimed.push({ file_id: file.file_id, source_path: claimStagingPath(file) });
    } else if (inDownloads && mockDownloads.has(file.file_name.toLowerCase())) {
      claimed.add(file.file_id);
      report.claimed.push({ file_id: file.file_id, source_path: `${folder}\\${file.file_name}` });
    } else {
      report.missing.push(file.file_id);
    }
  }
  return report;
}

/** 平台整合包某个版本的清单摘要：与导入时同一份清单。CurseForge 的 Mod 数与大小后端经批量接口补齐，两种格式都报数。 */
function summarizeModpack(args: MockArgs): ModpackSummaryDto {
  const read = argReader("summarize_platform_modpack", args);
  const source: ModpackSource = {
    kind: "platform",
    platform: parsePlatform(read.string("platform")),
    project_id: nonBlank("project_id", read.string("projectId")),
    version_id: nonBlank("version_id", read.string("versionId")),
  };
  const pack = packOf(source);
  assertBodyClaimed(pack);
  const { manifest } = pack;
  return {
    format: pack.format,
    name: pack.name,
    version: pack.version,
    minecraft: pack.minecraft,
    loader: loaderDto(manifest.loader),
    pack_bytes: pack.packBytes,
    remote_files: manifest.remoteFiles,
    mod_files: manifest.modFiles,
    remote_bytes: manifest.remoteBytes,
    override_files: manifest.overrideFiles,
    override_bytes: manifest.overrideBytes,
  };
}

// ---------------------------------------------------------------- 导出

const KB = 1024;
const MB = 1_048_576;
/** 每个存档按这么大算：mock 的存档列表不带可打包的文件数与体积。 */
const MOCK_WORLD_BYTES = 40 * MB;
/** 索引文件本身按每条这么多字节估，产物体积 = 覆盖文件 + 索引。 */
const MOCK_INDEX_ENTRY_BYTES = 512;

/** 额外项的固定盘面：config 有内容，defaultconfigs 与 kubejs 不存在，存档按实例的存档数现算。 */
const MOCK_EXTRAS: Readonly<Record<Exclude<ExportExtra, "saves">, { files: number; bytes: number }>> = {
  config: { files: 37, bytes: 412 * KB },
  defaultconfigs: { files: 0, bytes: 0 },
  kubejs: { files: 0, bytes: 0 },
  resourcepacks: { files: 2, bytes: 18 * MB },
  shaderpacks: { files: 1, bytes: 3 * MB },
  options: { files: 1, bytes: 6 * KB },
};

const EXPORT_NAME_MAX = 64;
const EXPORT_VERSION_MAX = 32;
const EXPORT_SUMMARY_MAX = 256;
const DEFAULT_PACK_VERSION = "1.0.0";
const WRITABLE_LOADERS: readonly PackLoaderKind[] = ["fabric", "quilt", "forge", "neoforge"];
const LOADER_TITLE: Readonly<Record<string, string>> = { optifine: "OptiFine", liteloader: "LiteLoader" };

function unsupported(id: string, reason: string, message: string, loader?: string): IpcError {
  return new IpcError("modpack_export_unsupported", message, loader === undefined ? { id, reason } : { id, reason, loader });
}

function instanceDtoOf(id: string): InstanceDto {
  const list = instanceHandlers.list_instances({}) as InstanceDto[];
  const found = list.find((i) => i.id === id);
  if (found === undefined) throw new IpcError("version_not_installed", `本地未安装版本 ${id}`, { id });
  return found;
}

/** 与后端同一个规划顺序：未装完 -> 版本认不出 -> 加载器写不进去 -> Mod 分组。 */
function planExport(id: string): ModpackExportPreviewDto {
  if (!mockInstanceIds().includes(id)) throw new IpcError("version_not_installed", `本地未安装版本 ${id}`, { id });
  const dto = instanceDtoOf(id);
  if (dto.status === "broken" || dto.activity === "installing" || dto.activity === "repairing") {
    throw unsupported(id, "incomplete", "实例还没装完整，修复后再导出");
  }
  if (dto.mc_version === null) throw unsupported(id, "version_unknown", "认不出这个实例的游戏版本，没法写进整合包");
  const foreign = dto.loaders.find((l) => l.kind === "optifine" || l.kind === "liteloader");
  if (foreign !== undefined) {
    throw unsupported(id, "loader_unsupported", `整合包格式不支持 ${LOADER_TITLE[foreign.kind]}，只支持 Fabric、Quilt、Forge、NeoForge`, foreign.kind);
  }
  const writable = dto.loaders.filter((l) => (WRITABLE_LOADERS as readonly string[]).includes(l.kind));
  if (writable.length > 1) throw unsupported(id, "multiple_loaders", "这个实例装了不止一种加载器，整合包只能写一种");
  let loader: PackLoader | null = null;
  if (writable.length === 1) {
    const only = writable[0];
    if (only.version === null) throw unsupported(id, "loader_version_unknown", "认不出加载器版本，没法写进整合包");
    loader = { kind: only.kind as PackLoaderKind, version: only.version };
  }

  const mods = mockInstance(id);
  const dir = mockModsDir(id);
  const indexed: ExportIndexedModDto[] = [];
  const bundled: ExportBundledModDto[] = [];
  const disabled: ExportBundledModDto[] = [];
  const byPath = [...mods.files].sort((a, b) => (a.file_name < b.file_name ? -1 : a.file_name > b.file_name ? 1 : 0));
  for (const file of byPath) {
    const path = `${dir}/${file.file_name}`;
    // 只认 *.jar 与 *.jar.disabled：.old 备份与别的文件不进包
    if (file.file_name.endsWith(`.jar${DISABLED_SUFFIX}`)) {
      disabled.push({ file_name: file.file_name, path, size: file.size, reason: "disabled" });
      continue;
    }
    if (!file.file_name.endsWith(".jar")) continue;
    const row = mods.ledger.find((r) => r.file_name === ledgerKey(file.file_name));
    if (row !== undefined && row.platform === "modrinth") {
      indexed.push({ file_name: file.file_name, path, size: file.size, project_id: row.project_id, version_id: row.version_id, title: row.title, matched_by: "ledger" });
    } else {
      bundled.push({ file_name: file.file_name, path, size: file.size, reason: row !== undefined && row.platform === "curseforge" ? "curseforge" : "unlisted" });
    }
  }

  const worlds = mockInstanceWorlds(id);
  if (worlds === null) throw new Error(`mock 实例 ${id} 已就绪却读不出存档`);
  const extras: ExportExtraStatDto[] = EXPORT_EXTRAS.map((extra) => {
    const stat = extra === "saves" ? { files: worlds.length, bytes: worlds.length * MOCK_WORLD_BYTES } : MOCK_EXTRAS[extra];
    return { extra, files: stat.files, bytes: stat.bytes, default_selected: extra !== "saves" && stat.files > 0, skipped: [] };
  });

  const pack = dto.modpack;
  return {
    name: pack === null ? id : pack.name,
    version: pack === null || pack.version === null ? DEFAULT_PACK_VERSION : pack.version,
    minecraft: dto.mc_version,
    loader,
    indexed,
    bundled,
    disabled,
    extras,
    mod_skipped: [],
    disabled_skipped: [],
  };
}

function previewModpackExport(args: MockArgs): ModpackExportPreviewDto {
  return planExport(argReader("preview_modpack_export", args).string("id"));
}

const OPTION_KEYS = new Set(["destination", "name", "version", "summary", "extras", "include_disabled"]);

/** options 的形状由 serde 反序列化（deny_unknown_fields）：认不出来是 Tauri 的参数错误，到前端归一为 unknown。 */
function parseExportOptions(raw: unknown): ModpackExportOptions {
  const malformed = (why: string) => new IpcError("unknown", `invalid args \`options\` for command \`export_modpack\`: ${why}`);
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) throw malformed("expected struct ModpackExportOptionsArg");
  const o = raw as Record<string, unknown>;
  const stray = Object.keys(o).find((k) => !OPTION_KEYS.has(k));
  if (stray !== undefined) throw malformed(`unknown field \`${stray}\``);
  const text = (key: string): string => {
    const value = o[key];
    if (typeof value !== "string") throw malformed(`missing or invalid field \`${key}\``);
    return value;
  };
  const summary = o.summary;
  if (summary !== undefined && summary !== null && typeof summary !== "string") throw malformed("invalid field `summary`");
  const extras = o.extras;
  if (!Array.isArray(extras) || extras.some((e) => typeof e !== "string")) throw malformed("missing or invalid field `extras`");
  if (typeof o.include_disabled !== "boolean") throw malformed("missing or invalid field `include_disabled`");
  return {
    destination: text("destination"),
    name: text("name"),
    version: text("version"),
    summary: summary === undefined ? null : summary,
    extras: extras as ExportExtra[],
    include_disabled: o.include_disabled,
  };
}

function exportInvalid(field: string, rule: string, message: string, extra: Record<string, unknown> = {}): IpcError {
  return new IpcError("invalid_argument", message, { field, rule, ...extra });
}

/** 名称与版本号：去掉首尾空白后非空、不超长（按字符计）、不含控制字符。 */
function checkedText(field: "name" | "version", raw: string, max: number, label: string): string {
  const value = raw.trim();
  if (value === "") throw exportInvalid(field, "empty", `${label}不能为空`);
  if ([...value].length > max) throw exportInvalid(field, "too_long", `${label}最多 ${max} 个字符`);
  if (/\p{Cc}/u.test(value)) throw exportInvalid(field, "control_char", `${label}不能含控制字符`);
  return value;
}

/** 与后端同一套规则；父目录是否存在不查（浏览器里没有磁盘）。 */
function validateExport(id: string, options: ModpackExportOptions): ModpackExportOptions {
  if (id.trim() === "") throw exportInvalid("id", "empty", "实例 id 不能为空");
  const name = checkedText("name", options.name, EXPORT_NAME_MAX, "整合包名称");
  const version = checkedText("version", options.version, EXPORT_VERSION_MAX, "版本号");
  const summaryText = options.summary === null ? "" : options.summary.trim();
  if ([...summaryText].length > EXPORT_SUMMARY_MAX) throw exportInvalid("summary", "too_long", `简介最多 ${EXPORT_SUMMARY_MAX} 个字符`);
  const extras: ExportExtra[] = [];
  for (const raw of options.extras) {
    const known = EXPORT_EXTRAS.find((e) => e === raw);
    if (known === undefined) throw exportInvalid("extras", "unknown", `不认识的打包项 ${raw}`, { value: raw });
    if (!extras.includes(known)) extras.push(known);
  }
  const destination = options.destination;
  if (!/^[A-Za-z]:[\\/]/.test(destination) && !/^\\\\/.test(destination)) {
    throw exportInvalid("destination", "relative", `导出位置必须是绝对路径：${destination}`);
  }
  if (!destination.toLowerCase().endsWith(".mrpack")) throw exportInvalid("destination", "extension", `导出的文件扩展名必须是 .mrpack：${destination}`);
  return { destination, name, version, summary: summaryText === "" ? null : summaryText, extras, include_disabled: options.include_disabled };
}

/** 与后端同一份步骤表：每次导出都按这四步报全，没有 Mod 时认 Mod 那一步也照报。 */
const EXPORT_PLAN: readonly InstallPhase[] = ["export_hash", "export_identify", "export_pack", "export_verify"];

function cancelledCheck(taskId: string): void {
  if (isMockTaskCancelled(taskId)) throw new IpcError("cancelled", "已取消");
}

function exportModpack(args: MockArgs): Promise<ModpackExportOutcomeDto> {
  const read = argReader("export_modpack", args);
  const taskId = read.string("taskId");
  const id = read.string("id");
  const options = validateExport(id, parseExportOptions(args.options));
  // 与后端一样先核对游戏目录、再占实例、再登记任务：参数不合法或目录换了都不占任何东西，任务无论怎样结束都在最后放开
  ensureMockGameDir(args, "export_modpack");
  const hold = acquireMockInstance(id, "modifying");
  // 勾了存档时与后端一样先记下「存档正被整份读走」再查：恢复与启动要等它读完
  const includesSaves = options.extras.includes("saves");
  const unmark = includesSaves ? markMockSavesRead(id) : () => undefined;
  const release = () => {
    unmark();
    hold.release();
  };
  // 没勾存档的导出不碰 saves，与运行和备份都兼容；勾了就要等存档安定下来，否则包里是半截的
  if (includesSaves) {
    try {
      ensureMockSavesSettledForReading(id);
    } catch (error) {
      release();
      throw error;
    }
  }
  return runMockTask(taskId, async () => {
    // 与后端同一规则：实例不在、导不出这类拒绝在规划时报出，过了才报第一步，被拒的导出一步都不报
    const plan = planExport(id);
    phase(taskId, EXPORT_PLAN, "export_hash");
    emitCoreEvent({ task_id: taskId, kind: "stage", message: `读取 ${plan.indexed.length + plan.bundled.length} 个 Mod 文件` });
    await idleStep(taskId);
    phase(taskId, EXPORT_PLAN, "export_identify");
    emitCoreEvent({ task_id: taskId, kind: "stage", message: `向 Modrinth 查询 ${plan.indexed.length + plan.bundled.length} 个 Mod` });
    await idleStep(taskId);

    const extras = plan.extras.filter((e) => options.extras.includes(e.extra));
    const carried = options.include_disabled ? plan.disabled : [];
    const files = extras.reduce((n, e) => n + e.files, 0) + plan.bundled.length + carried.length;
    const bytes = extras.reduce((n, e) => n + e.bytes, 0) + [...plan.bundled, ...carried].reduce((n, m) => n + m.size, 0);
    phase(taskId, EXPORT_PLAN, "export_pack");
    // 与后端同一先后：查询回来、进了打包这一步之后才报哪些 Mod 不在 Modrinth 上，排在「打包」阶段文字之前
    if (plan.bundled.length > 0) emitCoreEvent({ task_id: taskId, kind: "warning", message: `${plan.bundled.length} 个 Mod 不在 Modrinth 上，已直接打包进文件` });
    emitCoreEvent({ task_id: taskId, kind: "stage", message: `打包 ${files} 个文件` });
    for (let quarter = 0; quarter <= 4; quarter++) {
      if (quarter > 0) await paceTick(taskId);
      emitCoreEvent({
        task_id: taskId,
        kind: "download",
        total: files,
        finished: Math.round((files * quarter) / 4),
        bytes: Math.round((bytes * quarter) / 4),
        total_bytes: bytes,
        transferred: 0,
        speed: quarter === 0 ? 0 : 8 * MB,
      });
    }
    phase(taskId, EXPORT_PLAN, "export_verify");
    emitCoreEvent({ task_id: taskId, kind: "stage", message: "校验导出的整合包" });
    await idleStep(taskId);
    emitCoreEvent({ task_id: taskId, kind: "stage", message: `已导出到 ${options.destination}` });
    return {
      path: options.destination,
      name: options.name,
      version: options.version,
      bytes: bytes + MOCK_INDEX_ENTRY_BYTES * (plan.indexed.length + 1),
      indexed_files: plan.indexed.length,
      override_files: files,
      bundled_mods: plan.bundled.map((m) => m.file_name),
      disabled_mods: carried.length,
      skipped: [...plan.mod_skipped, ...(options.include_disabled ? plan.disabled_skipped : []), ...extras.flatMap((e) => e.skipped)].sort(),
    };
  })
    .then((outcome) => {
      recordMockExport(outcome.path);
      return outcome;
    })
    .finally(release);
}

export const handlers: MockHandlers = {
  preview_modpack: previewModpack,
  summarize_platform_modpack: summarizeModpack,
  install_modpack: installModpack,
  claim_blocked_files: claimBlockedFiles,
  downloads_folder: () => MOCK_DOWNLOADS_DIR,
  preview_modpack_export: previewModpackExport,
  export_modpack: exportModpack,
};
