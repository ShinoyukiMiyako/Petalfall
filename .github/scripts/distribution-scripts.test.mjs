// 分发链路里三个 shell 脚本的离线用例。真正的上传与 ssh 推送要连云和香港机，由「分发试跑」工作流验证；
// 这里把假的 aws、ssh 放到 PATH 最前面，记下脚本怎么调它们，脚本出错时也绝不会连到真实的桶或香港机。
// 读回用 file:// 地址代替公网。
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash, randomBytes, randomInt } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { after, describe, it } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

const script = (name) => fileURLToPath(new URL(`./${name}`, import.meta.url));
const KNOWN_HOSTS = fileURLToPath(new URL("../petalfall-hk.known_hosts", import.meta.url));
const HK_HOST = "8.210.200.215";
const root = mkdtempSync(join(tmpdir(), "distribution-scripts-"));
after(() => rmSync(root, { recursive: true, force: true }));

let nextCase = 0;
function caseDir() {
  const dir = join(root, String(nextCase++));
  mkdirSync(dir);
  return dir;
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

// 假程序把参数一行一个追加到 $FAKE_LOG，每次调用以 --- 结尾
const FAKE_AWS = `#!/usr/bin/env bash
if [ "$1" = "--version" ]; then
  echo "$FAKE_AWS_VERSION"
  exit 0
fi
{
  echo "AWS_PROFILE=$AWS_PROFILE"
  echo "AWS_ACCESS_KEY_ID=$AWS_ACCESS_KEY_ID"
  echo "AWS_SECRET_ACCESS_KEY=$AWS_SECRET_ACCESS_KEY"
  printf '%s\\n' "$@"
  echo ---
} >> "$FAKE_LOG"
cp "$AWS_CONFIG_FILE" "$FAKE_LOG.config"
for arg in "$@"; do
  if [ -n "\${FAKE_AWS_FAIL_ENDPOINT:-}" ] && [ "$arg" = "$FAKE_AWS_FAIL_ENDPOINT" ]; then
    echo "An error occurred (FileAlreadyExists) when calling the PutObject operation: object already exists" >&2
    exit 254
  fi
done
echo '{"ETag": "fake"}'
`;

// 只认 check-download.sh 用到的形状：--output <文件>、--write-out <格式>，最后一个参数是地址。把 https://<主机>/<路径>
// 映射到 $FAKE_CURL_ROOT/<主机>/<路径>，没有这个文件就按 404 失败；每次调用的地址记进 $FAKE_LOG.curl，参数记进
// $FAKE_LOG.curl-args。$FAKE_CURL_PLAN 是空格分开的「退出码/HTTP 状态/耗时秒数」，第 n 项决定第 n 次调用怎么失败：
// 先往输出文件追加一截残片、把模拟时钟拨快这么多秒，再按格式写出 HTTP 状态并以这个退出码退出。
// 成功时也只追加不覆盖：截断输出文件是脚本的责任，假 curl 替它截断就测不出残片混进比对。
const FAKE_CURL = `#!/usr/bin/env bash
output=""
format=""
previous=""
for arg in "$@"; do
  case "$previous" in
    --output) output="$arg" ;;
    --write-out) format="$arg" ;;
  esac
  previous="$arg"
done
url="\${@: -1}"
echo "$url" >> "$FAKE_LOG.curl"
{
  printf '%s\\n' "$@"
  echo ---
} >> "$FAKE_LOG.curl-args"
call="$(wc -l < "$FAKE_LOG.curl")"
read -ra plan <<< "\${FAKE_CURL_PLAN:-}"
report() {
  out=\${format//'%{http_code}'/$1}
  out=\${out//'%{speed_download}'/1048576}
  out=\${out//'%{time_total}'/0.5}
  printf '%s' "$out"
}
if [ "$call" -le "\${#plan[@]}" ]; then
  IFS=/ read -r code http took <<< "\${plan[call - 1]}"
  echo $(( $(cat "$FAKE_CLOCK") + took )) > "$FAKE_CLOCK"
  printf 'partial' >> "$output"
  report "$http"
  echo "curl: ($code) 按计划失败" >&2
  exit "$code"
fi
served="$FAKE_CURL_ROOT/\${url#https://}"
if [ ! -f "$served" ]; then
  report 404
  echo "curl: (22) The requested URL returned error: 404" >&2
  exit 22
fi
cat "$served" >> "$output"
report 200
`;

// $FAKE_SSH_EXITS 是空格分开的退出码，第 n 项让第 n 次调用读完 stdin 后以它退出，用完了再按 $FAKE_SSH_REPLY 回答
const FAKE_SSH = `#!/usr/bin/env bash
{
  printf '%s\\n' "$@"
  echo ---
} >> "$FAKE_LOG"
call="$(grep -c '^---$' "$FAKE_LOG")"
read -ra planned <<< "\${FAKE_SSH_EXITS:-}"
previous=""
for arg in "$@"; do
  case "$arg" in
    UserKnownHostsFile=*) cp "\${arg#UserKnownHostsFile=}" "$FAKE_LOG.known_hosts" ;;
  esac
  if [ "$previous" = "-i" ]; then
    # 真 ssh 读不到 -i 指的私钥时只打警告，接着因为没有可用的身份认证失败，以 255 退出
    if [ ! -f "$arg" ]; then
      echo "Warning: Identity file $arg not accessible: No such file or directory." >&2
      exit 255
    fi
    cp "$arg" "$FAKE_LOG.key"
    stat -c %a "$arg" > "$FAKE_LOG.mode"
  fi
  previous="$arg"
done
cat > "$FAKE_LOG.stdin"
if [ "$call" -le "\${#planned[@]}" ]; then
  echo "假 ssh：第 $call 次按计划以 \${planned[call - 1]} 退出" >&2
  exit "\${planned[call - 1]}"
fi
case "$FAKE_SSH_REPLY" in
  echo) sha256sum < "$FAKE_LOG.stdin" | cut -d' ' -f1 ;;
  noisy) echo "welcome"; sha256sum < "$FAKE_LOG.stdin" | cut -d' ' -f1 ;;
  reject) echo "put-manifest: 拒收：版本没有比现有清单新" >&2; exit 1 ;;
  *) echo "$FAKE_SSH_REPLY" ;;
esac
`;

// 重试间隔不真等：记下秒数，把模拟时钟拨快这么多
const FAKE_SLEEP = `#!/usr/bin/env bash
if [ "$#" -ne 1 ] || ! [[ "$1" =~ ^[0-9]+$ ]]; then
  echo "假 sleep 只认一个整数秒数，收到：$*" >&2
  exit 2
fi
echo "$1" >> "$FAKE_LOG.sleep"
echo $(( $(cat "$FAKE_CLOCK") + $1 )) > "$FAKE_CLOCK"
`;

// 只认 upload-to-mirrors.sh 的写法：timeout --kill-after=<秒> <秒> <命令…>，两个秒数记进 $FAKE_LOG.timeout。
// 命令里有一个参数等于 $FAKE_TIMEOUT_ENDPOINT 时，照样跑一遍命令（假 aws 记下这次调用），再把模拟时钟拨快整个时限、
// 以 $FAKE_TIMEOUT_STATUS（默认 124，补发 KILL 时是 137）退出，相当于传到时限被掐断；否则原样交给命令，退出码也是它的。
const FAKE_TIMEOUT = `#!/usr/bin/env bash
if [ "$#" -lt 3 ] || ! [[ "$1" =~ ^--kill-after=[0-9]+$ ]] || ! [[ "$2" =~ ^[0-9]+$ ]]; then
  echo "假 timeout 只认 --kill-after=<秒> <整数秒> <命令>，收到：$*" >&2
  exit 2
fi
echo "\${1#--kill-after=} $2" >> "$FAKE_LOG.timeout"
limit="$2"
shift 2
for arg in "$@"; do
  if [ -n "\${FAKE_TIMEOUT_ENDPOINT:-}" ] && [ "$arg" = "$FAKE_TIMEOUT_ENDPOINT" ]; then
    "$@" > /dev/null 2>&1 || true
    echo $(( $(cat "$FAKE_CLOCK") + limit )) > "$FAKE_CLOCK"
    exit "\${FAKE_TIMEOUT_STATUS:-124}"
  fi
done
exec "$@"
`;

// 脚本按 date +%s 算时限，这里读模拟时钟；别的用法说明脚本改了取时间的方式，用例要跟着改。
// 给了 $FAKE_DATE_STEP 时每读一次就把时钟拨快这么多秒，模拟两次取时间之间跨过了整秒
const FAKE_DATE = `#!/usr/bin/env bash
if [ "$#" -ne 1 ] || [ "$1" != "+%s" ]; then
  echo "假 date 只认 +%s，收到：$*" >&2
  exit 2
fi
cat "$FAKE_CLOCK"
if [ -n "\${FAKE_DATE_STEP:-}" ]; then
  echo $(( $(cat "$FAKE_CLOCK") + FAKE_DATE_STEP )) > "$FAKE_CLOCK"
fi
`;

function fakeBin() {
  const dir = caseDir();
  const bin = join(dir, "bin");
  mkdirSync(bin);
  writeFileSync(join(bin, "aws"), FAKE_AWS, { mode: 0o755 });
  writeFileSync(join(bin, "ssh"), FAKE_SSH, { mode: 0o755 });
  writeFileSync(join(bin, "curl"), FAKE_CURL, { mode: 0o755 });
  writeFileSync(join(bin, "sleep"), FAKE_SLEEP, { mode: 0o755 });
  writeFileSync(join(bin, "date"), FAKE_DATE, { mode: 0o755 });
  writeFileSync(join(bin, "timeout"), FAKE_TIMEOUT, { mode: 0o755 });
  writeFileSync(join(dir, "clock"), `${1_700_000_000 + randomInt(0, 100_000_000)}\n`);
  return { dir, log: join(dir, "calls.log") };
}

// Windows 上环境变量名不分大小写，process.env 里可能叫 Path；只留一个，免得子进程拿到两份
function envWithFakes(dir, log, extra) {
  const env = {};
  let path = "";
  for (const [name, value] of Object.entries(process.env)) {
    if (name.toUpperCase() === "PATH") path = value;
    else env[name] = value;
  }
  return { ...env, PATH: `${join(dir, "bin")}${delimiter}${path}`, FAKE_LOG: log, FAKE_CLOCK: join(dir, "clock"), ...extra };
}

const bash = (name, args, env) => spawnSync("bash", [script(name), ...args], { encoding: "utf8", env });
const calls = (log) =>
  existsSync(log)
    ? readFileSync(log, "utf8")
        .split("---\n")
        .filter((call) => call !== "")
        .map((call) => call.split("\n").slice(0, -1))
    : [];
const sleeps = (log) => (existsSync(`${log}.sleep`) ? readFileSync(`${log}.sleep`, "utf8").split("\n").slice(0, -1) : []);
const option = (args, name) => {
  const at = args.indexOf(name);
  assert.notEqual(at, -1, `参数里没有 ${name}：${args.join(" ")}`);
  return args[at + 1];
};

describe("check-download.sh", () => {
  function pair(remoteBytes, localBytes = remoteBytes) {
    const dir = caseDir();
    const remote = join(dir, "remote.bin");
    const local = join(dir, "local.bin");
    writeFileSync(remote, remoteBytes);
    writeFileSync(local, localBytes);
    return { url: pathToFileURL(remote).href, local };
  }

  it("字节相同时退出 0，打印大小和 sha256", () => {
    const bytes = randomBytes(randomInt(1, 256 * 1024));
    const { url, local } = pair(bytes);
    const result = bash("check-download.sh", ["hk", url, local], process.env);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, new RegExp(`^hk 读回一致：${bytes.length} 字节，sha256 ${sha256(bytes)}，`));
  });

  it("大小相同、内容不同时退出 1：只比大小挡不住被换掉的包", () => {
    const bytes = randomBytes(randomInt(1, 4096));
    const changed = Buffer.from(bytes);
    changed[randomInt(0, changed.length)] ^= 0xff;
    const { url, local } = pair(changed, bytes);
    const result = bash("check-download.sh", ["cn", url, local], process.env);
    assert.equal(result.status, 1, result.stdout);
    assert.equal(result.stderr, `::error::cn 读回的 sha256 是 ${sha256(changed)}，本地是 ${sha256(bytes)}：${url}\n`);
  });

  it("大小不同时退出 1", () => {
    const bytes = randomBytes(randomInt(2, 4096));
    const { url, local } = pair(bytes.subarray(1), bytes);
    const result = bash("check-download.sh", ["global", url, local], process.env);
    assert.equal(result.status, 1, result.stdout);
    assert.equal(result.stderr, `::error::global 读回 ${bytes.length - 1} 字节，本地 ${bytes.length} 字节：${url}\n`);
  });

  it("地址取不到时退出非 0，不打印读回一致", () => {
    const dir = caseDir();
    const local = join(dir, "local.bin");
    writeFileSync(local, randomBytes(16));
    const result = bash("check-download.sh", ["github", pathToFileURL(join(dir, "missing.bin")).href, local], process.env);
    assert.notEqual(result.status, 0);
    assert.doesNotMatch(result.stdout, /读回一致/);
  });

  it("参数不是三到五个时退出 2 并提示用法", () => {
    const { url, local } = pair(randomBytes(16));
    for (const args of [["hk", url], ["hk", url, local, "600", "error", "extra"]]) {
      const result = bash("check-download.sh", args, process.env);
      assert.equal(result.status, 2, args.join(" "));
      assert.match(result.stderr, /用法：check-download\.sh/);
    }
  });

  // 0 交给 --max-time 是不限时；前导零、负数、小数都说明调用方算错了
  it("时限不是正整数秒、注解级别不是 error 或 warning 时退出 2，不下载", () => {
    const { url, local } = pair(randomBytes(16));
    for (const budget of ["0", "00", "060", "-5", "1.5", "", "abc", " 60"]) {
      const result = bash("check-download.sh", ["hk", url, local, budget], process.env);
      assert.equal(result.status, 2, JSON.stringify(budget));
      assert.equal(result.stderr, `::error::时限应为正整数秒，实际是 ${budget}\n`);
      assert.equal(result.stdout, "");
    }
    for (const level of ["notice", "ERROR", "", "warn"]) {
      const result = bash("check-download.sh", ["hk", url, local, "60", level], process.env);
      assert.equal(result.status, 2, JSON.stringify(level));
      assert.equal(result.stderr, `::error::注解级别只能是 error 或 warning，实际是 ${level}\n`);
      assert.equal(result.stdout, "");
    }
  });

  // 下面几条走假 curl：它按 plan 先失败几次，之后从 https://dl.petalfall.org/<路径> 给出和本地相同的字节
  function flaky(plan) {
    const { dir, log } = fakeBin();
    const bytes = randomBytes(randomInt(1, 64 * 1024));
    const path = `dev/0.1.0-dev.${randomInt(1, 100_000)}-${randomInt(1, 2 ** 40)}-${randomInt(1, 5)}/Petalfall-Installer.exe`;
    const served = join(dir, "public", "dl.petalfall.org", ...path.split("/"));
    mkdirSync(join(served, ".."), { recursive: true });
    writeFileSync(served, bytes);
    const local = join(dir, "local.bin");
    writeFileSync(local, bytes);
    const env = envWithFakes(dir, log, { FAKE_CURL_ROOT: join(dir, "public"), FAKE_CURL_PLAN: plan.join(" ") });
    return { url: `https://dl.petalfall.org/${path}`, local, log, env, bytes, served };
  }
  const curlCalls = (log) => calls(`${log}.curl-args`);
  const maxTimes = (log) => curlCalls(log).map((args) => option(args, "--max-time"));

  it("传到一半被重置（56）两次、第三次成功时退出 0：每次从空文件重来，--max-time 按 600 秒总时限剩下的给", () => {
    const { url, local, log, env, bytes } = flaky(["56/200/40", "56/200/40"]);
    const result = bash("check-download.sh", ["cn", url, local], env);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, new RegExp(`^cn 读回一致：${bytes.length} 字节，sha256 ${sha256(bytes)}，用时 0\\.5 秒，1\\.00 MiB/s：`));
    const attempts = curlCalls(log);
    assert.equal(attempts.length, 3);
    // 600；600 - 40 - 5；600 - 2 × (40 + 5)
    assert.deepEqual(maxTimes(log), ["600", "555", "510"]);
    for (const args of attempts) {
      assert.equal(option(args, "--connect-timeout"), "30");
      assert.equal(option(args, "--speed-limit"), "1024");
      assert.equal(option(args, "--speed-time"), "60");
      // curl 自己再重试会让每次的 --max-time 重新计时，总时限就压不住了
      assert.equal(args.includes("--retry"), false, args.join(" "));
      assert.equal(args.at(-1), url);
    }
    assert.deepEqual(sleeps(log), ["5", "5"]);
    assert.equal(result.stderr.match(/::warning::cn 第 [12] 次读回失败（curl 退出 56，HTTP 200），5 秒后重试：/g).length, 2);
  });

  it("18、28、35、52、56、92 和 curl --retry 认的那几个 HTTP 状态都按暂时性错误重试", () => {
    const plans = [
      ...[18, 28, 35, 52, 56, 92].map((code) => `${code}/000/0`),
      ...[408, 429, 500, 502, 503, 504, 522, 524].map((http) => `22/${http}/0`),
    ];
    for (const plan of plans) {
      const { url, local, log, env } = flaky([plan]);
      const result = bash("check-download.sh", ["hk", url, local], env);
      assert.equal(result.status, 0, `${plan}：${result.stderr}`);
      assert.equal(curlCalls(log).length, 2, plan);
      assert.deepEqual(sleeps(log), ["5"], plan);
    }
  });

  it("404（--fail 下退出 22）、别的 4xx、501 和解析不了主机名都立即失败：只调一次 curl，不等待", () => {
    const missing = flaky([]);
    rmSync(missing.served);
    const cases = [
      { ...missing, code: 22, http: "404" },
      { ...flaky(["22/403/0"]), code: 22, http: "403" },
      { ...flaky(["22/501/0"]), code: 22, http: "501" },
      { ...flaky(["6/000/0"]), code: 6, http: "000" },
    ];
    for (const { url, local, log, env, code, http } of cases) {
      const result = bash("check-download.sh", ["global", url, local], env);
      assert.equal(result.status, code, `${http}：${result.stderr}`);
      assert.equal(curlCalls(log).length, 1, http);
      assert.deepEqual(sleeps(log), [], http);
      assert.match(result.stderr, new RegExp(`::error::global 读回失败（curl 退出 ${code}，HTTP ${http}），不是暂时性错误，不重试：`));
      assert.doesNotMatch(result.stdout, /读回一致/);
    }
  });

  it("一直是暂时性错误时试满 4 次就退出，退出码是最后一次的", () => {
    const { url, local, log, env } = flaky(["35/000/0", "18/200/0", "52/000/0", "56/200/0"]);
    const result = bash("check-download.sh", ["cn", url, local], env);
    assert.equal(result.status, 56, result.stderr);
    assert.deepEqual(maxTimes(log), ["600", "595", "590", "585"]);
    assert.deepEqual(sleeps(log), ["5", "5", "5"]);
    assert.match(result.stderr, /::error::cn 读回 4 次都失败（最后一次 curl 退出 56，HTTP 200）：/);
    assert.doesNotMatch(result.stdout, /读回一致/);
  });

  it("剩下的时间不够一次像样的尝试就不再重试，次数没用完也一样：单次调用压在 publish-hk 的作业时限以内", () => {
    const { url, local, log, env } = flaky(["56/200/300", "56/200/250"]);
    const result = bash("check-download.sh", ["github", url, local], env);
    assert.equal(result.status, 56, result.stderr);
    // 第二次只给 600 - 300 - 5；它失败时已过 555 秒，等完间隔只剩 40 秒，不到 60 秒就不试第三次
    assert.deepEqual(maxTimes(log), ["600", "295"]);
    assert.deepEqual(sleeps(log), ["5"]);
    assert.match(result.stderr, /::error::github 读回失败（curl 退出 56，HTTP 200），600 秒的时限等完重试间隔只剩 40 秒，不再重试：/);
  });

  it("等完间隔恰好剩 60 秒时还试一次、--max-time 给 60，剩 59 秒就不试", () => {
    const enough = flaky(["28/000/535"]);
    const retried = bash("check-download.sh", ["cn", enough.url, enough.local], enough.env);
    assert.equal(retried.status, 0, retried.stderr);
    assert.deepEqual(maxTimes(enough.log), ["600", "60"]);

    const short = flaky(["28/000/536"]);
    const given = bash("check-download.sh", ["cn", short.url, short.local], short.env);
    assert.equal(given.status, 28, given.stderr);
    assert.deepEqual(maxTimes(short.log), ["600"]);
    assert.deepEqual(sleeps(short.log), []);
    assert.match(given.stderr, /600 秒的时限等完重试间隔只剩 59 秒，不再重试：/);
  });

  it("给了时限就按它算：--max-time 从它起，剩得不够 60 秒同样不再试", () => {
    const budget = randomInt(200, 500);
    const { url, local, log, env } = flaky(["56/200/30", `28/000/${budget - 30 - 5 - 59}`]);
    const result = bash("check-download.sh", ["cn", url, local, String(budget)], env);
    assert.equal(result.status, 28, result.stderr);
    assert.deepEqual(maxTimes(log), [String(budget), String(budget - 35)]);
    assert.match(result.stderr, new RegExp(`::error::cn 读回失败（curl 退出 28，HTTP 000），${budget} 秒的时限等完重试间隔只剩 54 秒，不再重试：`));

    const passed = flaky(["56/200/10"]);
    const ok = bash("check-download.sh", ["hk", passed.url, passed.local, "120"], passed.env);
    assert.equal(ok.status, 0, ok.stderr);
    assert.deepEqual(maxTimes(passed.log), ["120", "105"]);
  });

  it("时限只给 1 秒、算到时已跨过一秒时 --max-time 仍给 1，不交出不限时的 0", () => {
    const { url, local, log, env } = flaky([]);
    const result = bash("check-download.sh", ["global", url, local, "1"], { ...env, FAKE_DATE_STEP: "1" });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(maxTimes(log), ["1"]);
  });

  // 允许缺的桶读回失败时作业照样成功，错误注解会让一次成功的运行看着像出了事
  it("注解级别给 warning 时，各种失败都只打警告，退出码不变", () => {
    const missing = flaky([]);
    rmSync(missing.served);
    const changed = flaky([]);
    writeFileSync(changed.served, Buffer.concat([changed.bytes, Buffer.from("x")]));
    const cases = [
      { ...missing, code: 22, pattern: /^::warning::cn 读回失败（curl 退出 22，HTTP 404），不是暂时性错误，不重试：/m },
      { ...flaky(["35/000/0", "18/200/0", "52/000/0", "56/200/0"]), code: 56, pattern: /^::warning::cn 读回 4 次都失败（最后一次 curl 退出 56，HTTP 200）：/m },
      { ...flaky(["56/200/300", "56/200/250"]), code: 56, pattern: /^::warning::cn 读回失败（curl 退出 56，HTTP 200），600 秒的时限等完重试间隔只剩 40 秒，不再重试：/m },
      { ...changed, code: 1, pattern: /^::warning::cn 读回 \d+ 字节，本地 \d+ 字节：/m },
    ];
    for (const { url, local, env, code, pattern } of cases) {
      const result = bash("check-download.sh", ["cn", url, local, "600", "warning"], env);
      assert.equal(result.status, code, result.stderr);
      assert.match(result.stderr, pattern);
      assert.doesNotMatch(result.stderr, /::error::/);
    }
    const { url, local } = pair(randomBytes(64), randomBytes(64));
    const differs = bash("check-download.sh", ["cn", url, local, "600", "warning"], process.env);
    assert.equal(differs.status, 1);
    assert.match(differs.stderr, /^::warning::cn 读回的 sha256 是 [0-9a-f]{64}，本地是 [0-9a-f]{64}：/);
  });
});

describe("upload-to-mirrors.sh", () => {
  const VARS = ["OSS_CI_ACCESS_KEY_ID", "OSS_CI_ACCESS_KEY_SECRET", "R2_CI_ACCESS_KEY_ID", "R2_CI_SECRET_ACCESS_KEY", "R2_ACCOUNT_ID"];
  const KEY = `dev/0.1.0-dev.${randomInt(1, 100_000)}-${randomInt(1, 2 ** 40)}-${randomInt(1, 5)}/sample.bin`;

  function setup(versionLine = `aws-cli/2.${randomInt(0, 40)}.${randomInt(0, 40)} Python/3.13.0 Linux/6.8.0 exe/x86_64.ubuntu.24`) {
    const { dir, log } = fakeBin();
    const file = join(dir, "sample.bin");
    const bytes = randomBytes(randomInt(1, 64 * 1024));
    writeFileSync(file, bytes);
    const credentials = {
      OSS_CI_ACCESS_KEY_ID: `LTAI${randomBytes(8).toString("hex")}`,
      OSS_CI_ACCESS_KEY_SECRET: randomBytes(15).toString("hex"),
      R2_CI_ACCESS_KEY_ID: randomBytes(16).toString("hex"),
      R2_CI_SECRET_ACCESS_KEY: randomBytes(32).toString("hex"),
      R2_ACCOUNT_ID: randomBytes(16).toString("hex"),
    };
    const publicRoot = join(dir, "public");
    // 在 Actions 里跑这些用例时 process.env 带着真的作业摘要路径，必须换掉，降级的提示不能写进构建作业的摘要
    const summary = join(dir, "summary.md");
    const record = join(dir, "mirror-record.tsv");
    const env = envWithFakes(dir, log, {
      RUNNER_TEMP: dir,
      FAKE_AWS_VERSION: versionLine,
      FAKE_CURL_ROOT: publicRoot,
      GITHUB_STEP_SUMMARY: summary,
      ...credentials,
    });
    return { dir, log, file, bytes, credentials, env, publicRoot, summary, record };
  }

  // 每个桶的 S3 端点（假 aws 按它决定哪一次上传报错）与客户端读回用的公网主机
  const targets = (account) => [
    { name: "cn", endpoint: "https://s3.oss-cn-hangzhou.aliyuncs.com", host: "petalfall-dl-cn.oss-cn-hangzhou.aliyuncs.com" },
    { name: "hk", endpoint: "https://s3.oss-cn-hongkong.aliyuncs.com", host: "petalfall-dl.oss-cn-hongkong.aliyuncs.com" },
    { name: "global", endpoint: `https://${account}.r2.cloudflarestorage.com`, host: "dl.petalfall.org" },
  ];

  // 让假 curl 在 https://<host>/<key> 上给出这些字节，相当于对象已经在桶里。假 aws 不往公网写，哪个桶读得回由用例决定
  function serve(publicRoot, host, bytes, key = KEY) {
    const path = join(publicRoot, host, ...key.split("/"));
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, bytes);
  }

  // 除 except 里的桶以外都读得回同样的字节
  function serveAll({ publicRoot, credentials, bytes }, except = [], key = KEY) {
    for (const { name, host } of targets(credentials.R2_ACCOUNT_ID)) {
      if (!except.includes(name)) serve(publicRoot, host, bytes, key);
    }
  }

  const timeouts = (log) => (existsSync(`${log}.timeout`) ? readFileSync(`${log}.timeout`, "utf8").split("\n").slice(0, -1) : []);
  const readbacks = (log) => (existsSync(`${log}.curl`) ? readFileSync(`${log}.curl`, "utf8").split("\n").slice(0, -1) : []);
  const readbackBudgets = (log) => calls(`${log}.curl-args`).map((args) => option(args, "--max-time"));
  const endpoints = (log) => calls(log).map((call) => call[call.indexOf("--endpoint-url") + 1]);
  const urlOf = (host, key = KEY) => `https://${host}/${key}`;

  it("同一个对象键传三个桶：杭州、香港用 OSS 凭据，R2 用 R2 凭据，带内容类型、缓存头、Content-MD5 与超时", () => {
    const fixture = setup();
    const { log, file, bytes, credentials: c, env, summary } = fixture;
    serveAll(fixture);
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 0, result.stderr);
    const md5 = createHash("md5").update(bytes).digest("base64");
    const put = (profile, id, secret, endpoint, region, bucket) => [
      `AWS_PROFILE=${profile}`,
      `AWS_ACCESS_KEY_ID=${id}`,
      `AWS_SECRET_ACCESS_KEY=${secret}`,
      "s3api",
      "put-object",
      "--endpoint-url",
      endpoint,
      "--region",
      region,
      "--bucket",
      bucket,
      "--key",
      KEY,
      "--body",
      file,
      "--content-type",
      "application/octet-stream",
      "--cache-control",
      "public, max-age=31536000, immutable",
      "--content-md5",
      md5,
      "--cli-connect-timeout",
      "30",
      "--cli-read-timeout",
      "120",
      "--no-cli-pager",
    ];
    assert.deepEqual(calls(log), [
      put("oss", c.OSS_CI_ACCESS_KEY_ID, c.OSS_CI_ACCESS_KEY_SECRET, "https://s3.oss-cn-hangzhou.aliyuncs.com", "cn-hangzhou", "petalfall-dl-cn"),
      put("oss", c.OSS_CI_ACCESS_KEY_ID, c.OSS_CI_ACCESS_KEY_SECRET, "https://s3.oss-cn-hongkong.aliyuncs.com", "cn-hongkong", "petalfall-dl"),
      put("r2", c.R2_CI_ACCESS_KEY_ID, c.R2_CI_SECRET_ACCESS_KEY, `https://${c.R2_ACCOUNT_ID}.r2.cloudflarestorage.com`, "auto", "petalfall-dl"),
    ]);
    assert.equal(
      readFileSync(`${log}.config`, "utf8"),
      [
        "[profile oss]",
        "request_checksum_calculation = when_required",
        "response_checksum_validation = when_required",
        "retry_mode = standard",
        "max_attempts = 5",
        "s3 =",
        "  addressing_style = virtual",
        "",
        "[profile r2]",
        "request_checksum_calculation = when_required",
        "response_checksum_validation = when_required",
        "retry_mode = standard",
        "max_attempts = 5",
        "",
      ].join("\n"),
    );
    // 配置文件里只有设置，凭据只走每条命令的环境变量
    for (const secret of Object.values(c)) assert.doesNotMatch(readFileSync(`${log}.config`, "utf8"), new RegExp(secret));
    // 每个桶 600 秒：put-object 最多 480 秒、--kill-after 10 秒；传完马上按公网地址读回，读回拿到的是这个桶剩下的时间
    assert.deepEqual(timeouts(log), ["10 480", "10 480", "10 480"]);
    assert.deepEqual(readbacks(log), targets(c.R2_ACCOUNT_ID).map(({ host }) => urlOf(host)));
    assert.deepEqual(readbackBudgets(log), ["600", "600", "600"]);
    for (const { name, host } of targets(c.R2_ACCOUNT_ID)) {
      assert.match(result.stdout, new RegExp(`^${name} 读回一致：${bytes.length} 字节，sha256 ${sha256(bytes)}，`, "m"));
      assert.match(result.stdout, new RegExp(`^${name}：已传到 [a-z-]+/${KEY.replaceAll(".", "\\.")}，用时 \\d+ 秒$`, "m"), host);
    }
    // 不给上传记录就不写记录，也不碰作业摘要
    assert.equal(existsSync(fixture.record), false);
    assert.equal(existsSync(summary), false);
  });

  for (const [index, name] of ["cn", "hk", "global"].entries()) it(`put-object 成功、公网读回和本地不同时退出 1，后面的桶不再传：公共读或自定义域名配错时只有读回看得出来（${name}）`, () => {
    const fixture = setup();
    const { log, file, bytes, credentials: c, env, publicRoot } = fixture;
    const failing = targets(c.R2_ACCOUNT_ID)[index];
    serveAll(fixture, [name]);
    serve(publicRoot, failing.host, Buffer.concat([bytes, Buffer.from("x")]));
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(`::error::${name} 读回 ${bytes.length + 1} 字节，本地 ${bytes.length} 字节`));
    assert.match(result.stderr, new RegExp(`::error::${name}：put-object 成功，按公网地址读回却和本地文件对不上，见上面读回的报错\n$`));
    assert.equal(calls(log).length, index + 1);
    assert.doesNotMatch(result.stdout, new RegExp(`${name}：已传到`));
  });

  // 只出在某一个桶上的回归（公网主机查表、读回地址对不上）要每次都抓到，三个桶逐个测，不随机挑
  const BUCKETS = ["cn", "hk", "global"];

  for (const [index, name] of BUCKETS.entries()) it(`put-object 报错但公网上已是同样的字节时照样算传上，接着传后面的桶：响应丢了、aws-cli 重发撞上禁止覆盖写（${name}）`, () => {
    const fixture = setup();
    const { log, file, credentials: c, env } = fixture;
    const failing = targets(c.R2_ACCOUNT_ID)[index];
    assert.equal(failing.name, name);
    env.FAKE_AWS_FAIL_ENDPOINT = failing.endpoint;
    serveAll(fixture);
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stderr, /FileAlreadyExists/);
    assert.match(result.stdout, new RegExp(`${failing.name}：put-object 报错，但公网读回与本地一致，按已传上处理\n`));
    assert.deepEqual(endpoints(log), targets(c.R2_ACCOUNT_ID).map(({ endpoint }) => endpoint));
    assert.deepEqual(readbacks(log), targets(c.R2_ACCOUNT_ID).map(({ host }) => urlOf(host)));
  });

  for (const [index, name] of BUCKETS.entries()) it(`put-object 报错、公网上的字节和本地不同时退出 1，后面的桶不再传（${name}）`, () => {
    const fixture = setup();
    const { log, file, bytes, credentials: c, env, publicRoot } = fixture;
    const failing = targets(c.R2_ACCOUNT_ID)[index];
    assert.equal(failing.name, name);
    env.FAKE_AWS_FAIL_ENDPOINT = failing.endpoint;
    const changed = Buffer.from(bytes);
    changed[randomInt(0, changed.length)] ^= 0xff;
    serveAll(fixture, [name]);
    serve(publicRoot, failing.host, changed);
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(`::error::${failing.name} 读回的 sha256 是 ${sha256(changed)}，本地是 ${sha256(bytes)}`));
    assert.match(result.stderr, new RegExp(`::error::${failing.name}：put-object 失败，按公网地址读回也和本地文件对不上，见上面 aws 的报错\n$`));
    assert.equal(calls(log).length, index + 1);
    assert.doesNotMatch(result.stdout, new RegExp(`${failing.name}：已传到`));
  });

  for (const [index, name] of BUCKETS.entries()) it(`put-object 报错、公网上没有这个对象时退出 1，后面的桶不再传（${name}）`, () => {
    const fixture = setup();
    const { log, file, credentials: c, env } = fixture;
    const failing = targets(c.R2_ACCOUNT_ID)[index];
    assert.equal(failing.name, name);
    env.FAKE_AWS_FAIL_ENDPOINT = failing.endpoint;
    serveAll(fixture, [name]);
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(`::error::${failing.name}：put-object 失败，按公网地址读回也和本地文件对不上，见上面 aws 的报错\n$`));
    assert.equal(calls(log).length, index + 1);
    assert.deepEqual(readbacks(log), targets(c.R2_ACCOUNT_ID).slice(0, index + 1).map(({ host }) => urlOf(host)));
  });

  for (const [index, name] of BUCKETS.entries()) it(`put-object 超过 480 秒被掐断、公网上没有这个对象时退出 1，读回只拿到剩下的 120 秒（${name}）`, () => {
    const fixture = setup();
    const { log, file, credentials: c, env } = fixture;
    const failing = targets(c.R2_ACCOUNT_ID)[index];
    env.FAKE_TIMEOUT_ENDPOINT = failing.endpoint;
    serveAll(fixture, [name]);
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(`::error::${name}：put-object 超过 480 秒没有传完，按公网地址读回也和本地文件对不上\n$`));
    assert.equal(calls(log).length, index + 1);
    assert.deepEqual(readbackBudgets(log), [...Array(index).fill("600"), "120"]);
  });

  it("put-object 被时限掐断、公网上却已是同样的字节时按已传上处理：可能刚好在掐断前写完", () => {
    const fixture = setup();
    const { log, file, credentials: c, env } = fixture;
    env.FAKE_TIMEOUT_ENDPOINT = targets(c.R2_ACCOUNT_ID)[1].endpoint;
    serveAll(fixture);
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^hk：put-object 超过 480 秒没有传完，但公网读回与本地一致，按已传上处理$/m);
    assert.deepEqual(endpoints(log), targets(c.R2_ACCOUNT_ID).map(({ endpoint }) => endpoint));
    assert.deepEqual(readbackBudgets(log), ["600", "120", "600"]);
  });

  // aws-cli 不理 TERM 时 timeout 在 10 秒后补发 KILL，连自己一起被杀，退出码是 137 而不是 124
  it("timeout 补发 KILL、以 137 退出时同样按超时处理", () => {
    const fixture = setup();
    const { file, credentials: c, env } = fixture;
    env.FAKE_TIMEOUT_ENDPOINT = targets(c.R2_ACCOUNT_ID)[0].endpoint;
    env.FAKE_TIMEOUT_STATUS = "137";
    serveAll(fixture, ["cn"]);
    const failed = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(failed.status, 1, failed.stdout);
    assert.match(failed.stderr, /::error::cn：put-object 超过 480 秒没有传完，按公网地址读回也和本地文件对不上\n$/);

    serveAll(fixture);
    const passed = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(passed.status, 0, passed.stderr);
    assert.match(passed.stdout, /^cn：put-object 超过 480 秒没有传完，但公网读回与本地一致，按已传上处理$/m);
  });

  // 下面是给了上传记录的情形：dev 发版这样调，cn 传不上只降级；分发试跑不给记录，三个桶都必须过（见上面几条）
  const recordOf = (record) => readFileSync(record, "utf8");
  const INSTALLER_KEY = KEY.replace(/sample\.bin$/, "sample-setup.bin");

  it("给了上传记录、三个桶都传上时每个桶记一行 ok，作业摘要不写东西", () => {
    const fixture = setup();
    const { file, env, record, summary } = fixture;
    serveAll(fixture);
    const result = bash("upload-to-mirrors.sh", [KEY, file, record], env);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(recordOf(record), `cn\tok\t${KEY}\nhk\tok\t${KEY}\nglobal\tok\t${KEY}\n`);
    assert.equal(existsSync(summary), false);
    assert.doesNotMatch(result.stderr, /::(?:error|warning)::/);
  });

  it("dev 上 cn 超过时限没传上时只打警告、写进作业摘要、记为 failed，hk、global 照传；后面的文件不再传 cn", () => {
    const fixture = setup();
    const { log, file, bytes, credentials: c, env, record, summary } = fixture;
    const [cn, hk, global] = targets(c.R2_ACCOUNT_ID);
    env.FAKE_TIMEOUT_ENDPOINT = cn.endpoint;
    serveAll(fixture, ["cn"]);
    const first = bash("upload-to-mirrors.sh", [KEY, file, record], env);
    assert.equal(first.status, 0, first.stderr);
    assert.doesNotMatch(first.stderr, /::error::/);
    // 读回失败的注解也降成警告，作业成功时不挂错误
    assert.match(first.stderr, /^::warning::cn 读回失败（curl 退出 22，HTTP 404），不是暂时性错误，不重试：/m);
    assert.match(
      first.stderr,
      /^::warning::cn：put-object 超过 480 秒没有传完，按公网地址读回也和本地文件对不上。dev 通道允许缺 cn：sample\.bin 不算传上，后面的文件不再传 cn，清单只列传上了更新包的源$/m,
    );
    assert.equal(recordOf(record), `cn\tfailed\t${KEY}\nhk\tok\t${KEY}\nglobal\tok\t${KEY}\n`);
    assert.deepEqual(endpoints(log), [cn.endpoint, hk.endpoint, global.endpoint]);
    assert.match(
      readFileSync(summary, "utf8"),
      /^\*\*下载源降级\*\*：cn 没有传上 `sample\.bin`（put-object 超过 480 秒没有传完，按公网地址读回也和本地文件对不上，用时 \d+ 秒）。清单只列传上了更新包的源，其余源照发。\n$/,
    );

    // 同一份记录传下一个文件：cn 一次都不再试，直接记为 failed
    const installer = join(fixture.dir, "sample-setup.bin");
    writeFileSync(installer, bytes);
    for (const { host } of [hk, global]) serve(fixture.publicRoot, host, bytes, INSTALLER_KEY);
    const second = bash("upload-to-mirrors.sh", [INSTALLER_KEY, installer, record], env);
    assert.equal(second.status, 0, second.stderr);
    assert.match(second.stderr, /^::warning::cn 在本次运行里已经失败过，sample-setup\.bin 不再传 cn$/m);
    assert.deepEqual(endpoints(log).slice(3), [hk.endpoint, global.endpoint]);
    assert.deepEqual(readbacks(log).slice(3), [urlOf(hk.host, INSTALLER_KEY), urlOf(global.host, INSTALLER_KEY)]);
    assert.equal(
      recordOf(record),
      [`cn\tfailed\t${KEY}`, `hk\tok\t${KEY}`, `global\tok\t${KEY}`, `cn\tfailed\t${INSTALLER_KEY}`, `hk\tok\t${INSTALLER_KEY}`, `global\tok\t${INSTALLER_KEY}`, ""].join("\n"),
    );
    assert.match(readFileSync(summary, "utf8"), /\n\*\*下载源降级\*\*：cn 在本次运行里已经失败过，没有再传 `sample-setup\.bin`。\n$/);
  });

  it("dev 上 cn 的 put-object 报错、公网上也没有时同样降级；cn 传上了引导程序时，下一个文件照样传 cn", () => {
    const fixture = setup();
    const { log, file, bytes, credentials: c, env, record, summary } = fixture;
    const [cn] = targets(c.R2_ACCOUNT_ID);
    serveAll(fixture);
    assert.equal(bash("upload-to-mirrors.sh", [KEY, file, record], env).status, 0);

    env.FAKE_AWS_FAIL_ENDPOINT = cn.endpoint;
    const installer = join(fixture.dir, "sample-setup.bin");
    writeFileSync(installer, bytes);
    serveAll(fixture, ["cn"], INSTALLER_KEY);
    const result = bash("upload-to-mirrors.sh", [INSTALLER_KEY, installer, record], env);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(endpoints(log).length, 6);
    assert.match(result.stderr, /^::warning::cn：put-object 失败，按公网地址读回也和本地文件对不上，见上面 aws 的报错。dev 通道允许缺 cn：/m);
    assert.doesNotMatch(result.stderr, /::error::/);
    assert.equal(
      recordOf(record),
      [`cn\tok\t${KEY}`, `hk\tok\t${KEY}`, `global\tok\t${KEY}`, `cn\tfailed\t${INSTALLER_KEY}`, `hk\tok\t${INSTALLER_KEY}`, `global\tok\t${INSTALLER_KEY}`, ""].join("\n"),
    );
    assert.match(readFileSync(summary, "utf8"), /^\*\*下载源降级\*\*：cn 没有传上 `sample-setup\.bin`（put-object 失败，/);
  });

  for (const name of ["hk", "global"]) it(`dev 上 ${name} 没传上时照样退出 1，不记它的结果：只有 cn 可以缺`, () => {
    const fixture = setup();
    const { file, credentials: c, env, record, summary } = fixture;
    env.FAKE_TIMEOUT_ENDPOINT = targets(c.R2_ACCOUNT_ID).find((target) => target.name === name).endpoint;
    serveAll(fixture, [name]);
    const result = bash("upload-to-mirrors.sh", [KEY, file, record], env);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, new RegExp(`::error::${name}：put-object 超过 480 秒没有传完，按公网地址读回也和本地文件对不上\n$`));
    assert.match(result.stderr, new RegExp(`^::error::${name} 读回失败（curl 退出 22，HTTP 404）`, "m"));
    assert.doesNotMatch(recordOf(record), new RegExp(`^${name}\t`, "m"));
    assert.equal(existsSync(summary), false);
  });

  it("beta 上 cn 没传上时退出 1，不记它的结果：beta 一个桶都不许缺", () => {
    const fixture = setup();
    const { log, file, credentials: c, env, record, summary } = fixture;
    const key = `beta/1.${randomInt(0, 50)}.0-beta.${randomInt(1, 100_000)}-${randomInt(1, 2 ** 40)}-${randomInt(1, 5)}/sample.bin`;
    env.FAKE_TIMEOUT_ENDPOINT = targets(c.R2_ACCOUNT_ID)[0].endpoint;
    const result = bash("upload-to-mirrors.sh", [key, file, record], env);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /::error::cn：put-object 超过 480 秒没有传完，按公网地址读回也和本地文件对不上\n$/);
    assert.equal(calls(log).length, 1);
    assert.equal(recordOf(record), "");
    assert.equal(existsSync(summary), false);
  });

  it("dev 上不给上传记录时 cn 没传上也退出 1：分发试跑要看清每个桶", () => {
    const fixture = setup();
    const { log, file, credentials: c, env, record } = fixture;
    env.FAKE_TIMEOUT_ENDPOINT = targets(c.R2_ACCOUNT_ID)[0].endpoint;
    serveAll(fixture, ["cn"]);
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /::error::cn：put-object 超过 480 秒没有传完/);
    assert.equal(calls(log).length, 1);
    assert.equal(existsSync(record), false);
  });

  it("给了上传记录却没有作业摘要路径时退出非 0，一个桶都不传", () => {
    for (const value of [undefined, ""]) {
      const { log, file, env, record } = setup();
      if (value === undefined) delete env.GITHUB_STEP_SUMMARY;
      else env.GITHUB_STEP_SUMMARY = value;
      const result = bash("upload-to-mirrors.sh", [KEY, file, record], env);
      assert.notEqual(result.status, 0, String(value));
      assert.match(result.stderr, /给了上传记录就要有 GITHUB_STEP_SUMMARY/);
      assert.deepEqual(calls(log), []);
    }
  });

  it("aws-cli 不是 2.x 时退出 1，一个桶都不传", () => {
    for (const line of ["aws-cli/1.36.0 Python/3.12.0 Linux/6.8.0 botocore/1.36.0", "aws-cli/20.0.0 Python/3.13.0", ""]) {
      const { log, file, env } = setup(line);
      const result = bash("upload-to-mirrors.sh", [KEY, file], env);
      assert.equal(result.status, 1, line);
      assert.match(result.stderr, /::error::需要 aws-cli 2\.x/);
      assert.deepEqual(calls(log), []);
    }
  });

  it("缺任何一个凭据或账户 ID 时退出 1 并点名缺的是哪个，一个桶都不传", () => {
    for (const missing of VARS) {
      for (const value of [undefined, ""]) {
        const { log, file, env } = setup();
        if (value === undefined) delete env[missing];
        else env[missing] = value;
        const result = bash("upload-to-mirrors.sh", [KEY, file], env);
        assert.equal(result.status, 1, `${missing}=${value}`);
        assert.equal(result.stderr, `::error::作业所在的环境里没有配置 ${missing}\n`);
        assert.deepEqual(calls(log), []);
      }
    }
  });

  it("R2 账户 ID 格式不对时退出非 0，一个桶都不传", () => {
    const { log, file, env } = setup();
    env.R2_ACCOUNT_ID = env.R2_ACCOUNT_ID.toUpperCase();
    const result = bash("upload-to-mirrors.sh", [KEY, file], env);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /R2 账户 ID 应为 32 位小写十六进制/);
    assert.deepEqual(calls(log), []);
  });

  it("beta/ 下的对象键同样按原样传进三个桶", () => {
    const fixture = setup();
    const { log, file, env } = fixture;
    const key = `beta/1.${randomInt(0, 50)}.${randomInt(0, 50)}-beta.${randomInt(1, 100_000)}-${randomInt(1, 2 ** 40)}-${randomInt(1, 5)}/sample.bin`;
    serveAll(fixture, [], key);
    const result = bash("upload-to-mirrors.sh", [key, file], env);
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(
      calls(log).map((call) => call[call.indexOf("--key") + 1]),
      [key, key, key],
    );
  });

  it("对象键不在 dev/ 或 beta/ 下时退出非 0，一个桶都不传：R2 的凭据能写整个桶，写偏了没有任何桶规则兜底", () => {
    for (const key of ["stable/0.1.0/sample.bin", "Beta/1.0.0-beta.1-1/sample.bin", "betas/1.0.0-beta.1-1/sample.bin", "sample.bin", "dev/../sample.bin", "beta/../sample.bin", `/${KEY}`]) {
      const { log, file, env } = setup();
      const result = bash("upload-to-mirrors.sh", [key, file], env);
      assert.notEqual(result.status, 0, key);
      assert.match(result.stderr, /对象键应为 <通道>\/<目录>\/<文件名>，通道只能是 dev、beta/);
      assert.deepEqual(calls(log), []);
    }
  });

  // 第三个参数传成空串多半是工作流里的变量没设上，不能悄悄当成不给记录
  it("参数不是两到三个、或上传记录是空串时退出 2 并提示用法，一个桶都不传", () => {
    const { log, file, env, record } = setup();
    for (const args of [[file], [KEY, file, record, "extra"], [KEY, file, ""]]) {
      const result = bash("upload-to-mirrors.sh", args, env);
      assert.equal(result.status, 2, JSON.stringify(args));
      assert.match(result.stderr, /用法：upload-to-mirrors\.sh <对象键> <本地文件> \[上传记录\]/);
    }
    assert.deepEqual(calls(log), []);
  });
});

describe("push-hk-manifest.sh", () => {
  const KEY_TEXT = ["-----BEGIN OPENSSH PRIVATE KEY-----", randomBytes(48).toString("base64"), "-----END OPENSSH PRIVATE KEY-----"];

  function setup(reply, key = `${KEY_TEXT.join("\n")}\n`) {
    const { dir, log } = fakeBin();
    const runnerTemp = join(dir, "runner-temp");
    mkdirSync(runnerTemp);
    const manifest = join(dir, "latest.json");
    const bytes = Buffer.from(JSON.stringify({ version: `0.1.0-dev.${randomInt(1, 100_000)}`, nonce: randomBytes(8).toString("hex") }, null, 2));
    writeFileSync(manifest, bytes);
    const env = envWithFakes(dir, log, { RUNNER_TEMP: runnerTemp, HK_MANIFEST_SSH_KEY: key, FAKE_SSH_REPLY: reply });
    return { log, runnerTemp, manifest, bytes, env };
  }

  it("按钉死的主机公钥、只用这把私钥、不交互地把清单从 stdin 送过去，回显的 sha256 一致时退出 0", () => {
    const { log, runnerTemp, manifest, bytes, env } = setup("echo");
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, `香港清单已写入：sha256 ${sha256(bytes)}\n`);
    const [args, ...rest] = calls(log);
    assert.deepEqual(rest, []);
    assert.equal(args.at(-1), `petalfall-pub@${HK_HOST}`);
    const options = args.flatMap((arg, i) => (args[i - 1] === "-o" ? [arg] : []));
    assert.deepEqual(options.filter((o) => !o.startsWith("UserKnownHostsFile=")), [
      "IdentitiesOnly=yes",
      "IdentityAgent=none",
      "BatchMode=yes",
      "ConnectTimeout=30",
      "ServerAliveInterval=15",
      "ServerAliveCountMax=4",
      "StrictHostKeyChecking=yes",
      "GlobalKnownHostsFile=/dev/null",
    ]);
    assert.equal(options.filter((o) => o.startsWith("UserKnownHostsFile=")).length, 1);
    assert.equal(readFileSync(`${log}.known_hosts`, "utf8"), readFileSync(KNOWN_HOSTS, "utf8"));
    assert.ok(args.includes("-T"));
    assert.equal(args[args.indexOf("-i") + 1].startsWith(runnerTemp), true, args.join(" "));
    assert.deepEqual(readFileSync(`${log}.stdin`), bytes);
    assert.equal(readFileSync(`${log}.key`, "utf8"), `${KEY_TEXT.join("\n")}\n`);
    if (process.platform !== "win32") assert.equal(readFileSync(`${log}.mode`, "utf8"), "600\n");
    // 私钥用完即删
    assert.deepEqual(readdirSync(runnerTemp), []);
  });

  it("机密里的私钥带 CRLF、结尾没换行时，写出的文件是 LF 且以换行结尾", () => {
    const { log, manifest, env } = setup("echo", KEY_TEXT.join("\r\n"));
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(`${log}.key`, "utf8"), `${KEY_TEXT.join("\n")}\n`);
  });

  it("回显的 sha256 和本地不同时退出 1，私钥照样删掉", () => {
    const other = randomBytes(32).toString("hex");
    const { runnerTemp, manifest, bytes, env } = setup(other);
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.equal(result.status, 1);
    assert.equal(result.stderr, `::error::香港机回显的 sha256 是 ${other}，本地清单是 ${sha256(bytes)}\n`);
    assert.deepEqual(readdirSync(runnerTemp), []);
  });

  it("回显里除了 sha256 还有别的内容时退出 1：认不出脚本的回答就不能当成功", () => {
    const { runnerTemp, manifest, env } = setup("noisy");
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /::error::put-manifest 的输出应只有一行 sha256，实际是：welcome/);
    assert.deepEqual(readdirSync(runnerTemp), []);
  });

  it("香港机拒收时退出 1，不重试，私钥照样删掉", () => {
    const { log, runnerTemp, manifest, env } = setup("reject");
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /put-manifest: 拒收/);
    assert.doesNotMatch(result.stdout, /香港清单已写入/);
    assert.equal(calls(log).length, 1);
    assert.deepEqual(sleeps(log), []);
    assert.deepEqual(readdirSync(runnerTemp), []);
  });

  it("ssh 自身出错（255）一次后重试成功：同样的参数，清单从头再送一遍，私钥留到最后一次之后才删", () => {
    const { log, runnerTemp, manifest, bytes, env } = setup("echo");
    env.FAKE_SSH_EXITS = "255";
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.equal(result.status, 0, result.stderr);
    // stdout 仍然只有最后这一行，重试的提示走 stderr
    assert.equal(result.stdout, `香港清单已写入：sha256 ${sha256(bytes)}\n`);
    assert.match(result.stderr, /::warning::ssh 第 1 次以 255 退出（连接或认证出错），5 秒后重试\n/);
    const [first, second, ...rest] = calls(log);
    assert.deepEqual(rest, []);
    assert.deepEqual(second, first);
    // 假 ssh 每次重写 .stdin：第二次也收到完整清单，说明 stdin 每次从文件重接。私钥要是在重试前删了，
    // 假 ssh 后面几次都以 255 退出，上面的退出 0 就不成立
    assert.deepEqual(readFileSync(`${log}.stdin`), bytes);
    assert.equal(readFileSync(`${log}.key`, "utf8"), `${KEY_TEXT.join("\n")}\n`);
    assert.deepEqual(sleeps(log), ["5"]);
    assert.deepEqual(readdirSync(runnerTemp), []);
  });

  it("ssh 一直以 255 退出时试满 3 次后退出 255，私钥照样删掉", () => {
    const { log, runnerTemp, manifest, env } = setup("echo");
    env.FAKE_SSH_EXITS = "255 255 255";
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.equal(result.status, 255, result.stderr);
    assert.match(result.stderr, /::error::ssh 连了 3 次都以 255 退出（连接或认证出错），香港清单没有推上去\n$/);
    assert.doesNotMatch(result.stdout, /香港清单已写入/);
    assert.equal(calls(log).length, 3);
    assert.deepEqual(sleeps(log), ["5", "5"]);
    assert.deepEqual(readdirSync(runnerTemp), []);
  });

  it("put-manifest 拒收（1）或参数错误（2）时立即失败，只连一次", () => {
    for (const code of [1, 2]) {
      const { log, runnerTemp, manifest, env } = setup("echo");
      env.FAKE_SSH_EXITS = String(code);
      const result = bash("push-hk-manifest.sh", [manifest], env);
      assert.equal(result.status, code, result.stderr);
      assert.match(result.stderr, new RegExp(`::error::香港机上的 put-manifest 以 ${code} 退出，没有收下这份清单，见上面它的报错\n$`));
      assert.doesNotMatch(result.stdout, /香港清单已写入/);
      assert.equal(calls(log).length, 1, String(code));
      assert.deepEqual(sleeps(log), [], String(code));
      assert.deepEqual(readdirSync(runnerTemp), []);
    }
  });

  it("没有推送私钥时退出 1，不连香港机，也不留下私钥文件", () => {
    for (const value of [undefined, ""]) {
      const { log, runnerTemp, manifest, env } = setup("echo");
      if (value === undefined) delete env.HK_MANIFEST_SSH_KEY;
      else env.HK_MANIFEST_SSH_KEY = value;
      const result = bash("push-hk-manifest.sh", [manifest], env);
      assert.equal(result.status, 1, String(value));
      assert.equal(result.stderr, "::error::作业所在的环境里没有配置 HK_MANIFEST_SSH_KEY\n");
      assert.deepEqual(calls(log), []);
      assert.deepEqual(readdirSync(runnerTemp), []);
    }
  });

  it("清单文件为空时退出非 0，不连香港机：空文件说明前面生成清单出了错", () => {
    const { log, manifest, env } = setup("echo");
    writeFileSync(manifest, "");
    const result = bash("push-hk-manifest.sh", [manifest], env);
    assert.notEqual(result.status, 0);
    assert.deepEqual(calls(log), []);
  });

  it("参数不是恰好一个时退出 2 并提示用法", () => {
    const { env } = setup("echo");
    const result = bash("push-hk-manifest.sh", [], env);
    assert.equal(result.status, 2);
    assert.match(result.stderr, /用法：push-hk-manifest\.sh/);
  });
});

// 占位行或贴错格式（比如直接贴 ssh_host_ed25519_key.pub 的原文，ssh 会把 ssh-ed25519 当成主机名）时，
// ssh 要到预发布重建之后的推送才报 Host key verification failed；在 build 作业的脚本测试里就挡下。
describe("petalfall-hk.known_hosts", () => {
  it(`恰好一条非注释记录，是 ${HK_HOST} 的完整 ed25519 主机公钥`, () => {
    const records = readFileSync(KNOWN_HOSTS, "utf8")
      .split(/\r?\n/)
      .filter((line) => line.trim() !== "" && !line.trimStart().startsWith("#"));
    assert.equal(records.length, 1, `应恰好一条主机公钥记录，实际 ${records.length} 条`);
    const match = new RegExp(`^${HK_HOST.replaceAll(".", "\\.")} ssh-ed25519 ([A-Za-z0-9+/]+={0,2})$`).exec(records[0]);
    assert.notEqual(match, null, `记录应为「${HK_HOST} ssh-ed25519 <base64>」，实际：${records[0]}`);
    // 公钥 blob 是 string("ssh-ed25519") 加 string(32 字节公钥)，各带 4 字节大端长度；截断或多贴的字符在这里露出来
    const blob = Buffer.from(match[1], "base64");
    assert.equal(blob.toString("base64"), match[1]);
    assert.equal(blob.length, 4 + 11 + 4 + 32);
    assert.equal(blob.readUInt32BE(0), 11);
    assert.equal(blob.subarray(4, 15).toString("latin1"), "ssh-ed25519");
    assert.equal(blob.readUInt32BE(15), 32);
  });
});
