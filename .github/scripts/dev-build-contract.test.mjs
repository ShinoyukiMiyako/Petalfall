// .github/workflows/dev-build.yml 的契约。dev 分支一推送就发布，工作流只能靠真跑验证，下面这几处一旦走样，运行时不会有
// 任何报错：违反版本先后的包照样发给所有 dev 用户，载荷头或内嵌公钥不对的包照样发出去，持钥的作业按 registry 当下给的
// 元数据取签名 CLI，或者所有 dev 客户端拒收更新。放在 node 测试里，是因为 build 作业每次都在编译之前跑 node --test，
// 契约在 CI 上生效。
//
// 按行解析、取作业与步骤、找机密与令牌的规则见 workflow-lines.mjs：只看真正执行的行，结构行只许用解析得了的写法，
// 文件头和步骤上方的注释里也写着这些命令，拿全文找子串的话，删掉真正的那一行照样能过。

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { randomBytes, randomInt } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, describe, it } from 'node:test';

import * as workflow from './workflow-lines.mjs';

const { blockOf, codeOf, exposedOf, parseWorkflow, scriptOf, secretsIn, stepKeys, stepText, structure, tokenRefs } = workflow;

const REPO_ROOT = new URL('../../', import.meta.url);
const WORKFLOW = readFileSync(new URL('.github/workflows/dev-build.yml', REPO_ROOT), 'utf8').replace(/\r\n/g, '\n');

/** 允许的 flow 集合，整行比对。flow 集合能跨行、能嵌套，逐行看不出折成了什么，只放行这份工作流现有的这几处。 */
const FLOW_ALLOWED = ['branches: [dev]', 'runs-on: [self-hosted, Linux, X64, petalfall]', 'needs: [build, upload-cn]', 'needs: [build, publish]'];

const PARSED = parseWorkflow(WORKFLOW, FLOW_ALLOWED);
const LINES = PARSED.lines;
const CODE = codeOf(LINES);

const jobNames = () => workflow.jobNames(LINES);
const jobLines = (name) => workflow.jobLines(LINES, name);
const job = (name) => workflow.job(LINES, name);
const jobKeys = (name) => workflow.jobKeys(LINES, name);
const jobBlock = (name, key) => workflow.jobBlock(LINES, name, key);
const steps = (name) => workflow.steps(LINES, name);

/** 步骤去掉 shell 注释后的全文（保留缩进）：注释不执行，改措辞不该让契约挂。 */
const pinned = (step) => stepText(step).filter((line) => !line.trim().startsWith('#'));

/** 作业 name 里叫 stepName 的那一步的下标，找不到时直接失败。 */
function stepIndex(name, stepName) {
  const index = steps(name).findIndex((step) => stepKeys(step).includes(`name: ${stepName}`));
  assert.notEqual(index, -1, `${name} 作业里找不到「${stepName}」这一步`);
  return index;
}

const HEADS = 'node .github/scripts/release-heads.mjs';
const HEADS_FILE = '"$RUNNER_TEMP/release-heads.json"';
const BUILD_COMMAND = 'pnpm tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis';
const CARGO_TEST = 'cargo test --release --locked -p petalfall-setup-payload -p petalfall-release';
const SIGN_COMMAND = 'node "$TAURI_CLI" signer sign --app-version "$VERSION" "$BOOTSTRAP"';
const PUBKEY_LINE =
  "conf.plugins = { ...conf.plugins, updater: { pubkey: fs.readFileSync('../.github/dev-updater.pub', 'utf8').trim(), requireSignedVersion: true } };";

// 注入公钥那一步的全文。只拦 if、continue-on-error 拦不住同类写法：shell: "true {0}" 把脚本当参数交给 true，
// 脚本开头一句 exit 0、命令后面接 || true，注入都不执行，打出的包不带公钥，dev 用户收不到后续更新。所以键和脚本逐行钉死。
const INJECT_STEP = [
  '      - name: 写入开发版更新公钥',
  '        working-directory: app',
  '        run: |',
  '          node -e "',
  "            const fs = require('fs');",
  "            const path = 'src-tauri/tauri.conf.json';",
  "            const conf = JSON.parse(fs.readFileSync(path, 'utf8'));",
  `            ${PUBKEY_LINE}`,
  '            fs.writeFileSync(path, JSON.stringify(conf, null, 2));',
  '          "',
];

// 打包那一步的脚本全文（不含 shell 注释）。打包后的几条核对都写在这一步里，只找那几行拦不住同类写法：把它包进一个
// 从不调用的函数，或在前面插一句 exit 0，核对就不再执行。和注入那一步一样逐行钉死。
const PETALFALL_EXE = '"$CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/Petalfall.exe"';
const BUNDLE_SCRIPT = [
  'bundle_dir="$CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/bundle"',
  'rm -rf "$bundle_dir"',
  BUILD_COMMAND,
  `if ! grep -qF -e "$PETALFALL_MSA_CLIENT_ID" -- ${PETALFALL_EXE}; then`,
  'echo "::error::Petalfall.exe 里没有内嵌微软登录 client_id"',
  'exit 1',
  'fi',
  `if ! grep -qF -e "$PETALFALL_CURSEFORGE_API_KEY" -- ${PETALFALL_EXE}; then`,
  'echo "::error::Petalfall.exe 里没有内嵌 CurseForge API key"',
  'exit 1',
  'fi',
  `if grep -qF -e "jent_entropy_collector_alloc" -- ${PETALFALL_EXE}; then`,
  'echo "::error::Petalfall.exe 里编进了 aws-lc 的 CPU 抖动熵源"',
  'exit 1',
  'fi',
  'installer="$bundle_dir/nsis/Petalfall_${VERSION}_x64-setup.exe"',
  'test -f "$installer"',
  'echo "installer=$installer" >> "$GITHUB_OUTPUT"',
  'echo "installer-name=$(basename "$installer")" >> "$GITHUB_OUTPUT"',
];

// release-heads 检出之后的四步全文。同样的道理：命令后面接 || true、多一个 if，检查就不再挡构建，
// 违反版本先后的包照样发出去；dev 每次推送都发布，模式只能是 release。版本号的公式也钉在这里，build 不再自己算。
const RELEASE_HEADS_STEPS = [
  [
    '      - name: 算出 dev 版本号',
    '        id: version',
    '        working-directory: app',
    '        run: |',
    `          version="$(node -p "require('./src-tauri/tauri.conf.json').version")-dev.$GITHUB_RUN_NUMBER"`,
    '          echo "version=$version" >> "$GITHUB_OUTPUT"',
  ],
  ['      - name: 读出各通道已发布的版本', `        run: ${HEADS} read "$GITHUB_REPOSITORY" ${HEADS_FILE}`],
  [
    '      - name: 版本先后检查',
    '        env:',
    '          VERSION: ${{ steps.version.outputs.version }}',
    `        run: ${HEADS} gate dev release "$VERSION" ${HEADS_FILE}`,
  ],
  ['      - name: 检查数据格式号没有倒退', `        run: ${HEADS} data-format dev release ${HEADS_FILE} "$GITHUB_REPOSITORY"`],
];

// build 写进 tauri.conf.json 的必须就是 release-heads 查过的那个版本号：这里另算一遍（哪怕公式相同），
// 检查过的和发出去的就可能不是同一个。
const WRITE_VERSION_STEP = [
  '      - name: 写入 dev 版本号',
  '        id: version',
  '        working-directory: app',
  '        env:',
  '          VERSION: ${{ needs.release-heads.outputs.version }}',
  '        run: |',
  '          if [ -z "$VERSION" ]; then',
  '            echo "::error::release-heads 作业没有交出版本号"',
  '            exit 1',
  '          fi',
  '          node -e "',
  "            const fs = require('fs');",
  "            const path = 'src-tauri/tauri.conf.json';",
  "            const conf = JSON.parse(fs.readFileSync(path, 'utf8'));",
  '            conf.version = process.argv[1];',
  '            fs.writeFileSync(path, JSON.stringify(conf, null, 2));',
  '          " "$VERSION"',
  '          echo "version=$VERSION" >> "$GITHUB_OUTPUT"',
  `          echo "cli-version=$(node -p "require('@tauri-apps/cli/package.json').version")" >> "$GITHUB_OUTPUT"`,
];

// 构建后的两步核对。载荷头是启动器下载验签之后判断能不能装的唯一依据，两把公钥缺一把，偏好那个通道的用户就收不到更新；
// 核对不执行时一切照常打包上传，所以与注入那一步一样逐行钉死。
const PAYLOAD_STEP = [
  '      - name: 核对载荷头',
  '        id: payload',
  '        env:',
  '          VERSION: ${{ steps.version.outputs.version }}',
  '          BOOTSTRAP: ${{ steps.bootstrap.outputs.bootstrap }}',
  '        run: |',
  '          header="$(cargo run --release --locked -p petalfall-setup-payload --bin inspect-setup -- "$BOOTSTRAP")"',
  "          IFS=$'\\t' read -r magic header_version data_format caps <<< \"$header\"",
  `          ${HEADS} payload "$VERSION" "$magic" "$header_version" "$data_format" "$caps"`,
];

const PUBKEY_CHECK_STEP = [
  '      - name: 核对内嵌的更新公钥',
  '        run: |',
  '          exe="$CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/Petalfall.exe"',
  '          for pub in .github/dev-updater.pub .github/release-updater.pub; do',
  `            key="$(node -p "require('fs').readFileSync(process.argv[1], 'utf8').trim()" "$pub")"`,
  '            if [ -z "$key" ]; then',
  '              echo "::error::$pub 是空的"',
  '              exit 1',
  '            fi',
  '            if ! grep -qF -e "$key" -- "$exe"; then',
  '              echo "::error::Petalfall.exe 里没有内嵌 $pub 的更新公钥"',
  '              exit 1',
  '            fi',
  '          done',
];

// 取签名 CLI 那一步的全文：比对 integrity 的那几行一删，拿到的就是 registry 当下给的任意一份，下一步拿私钥去跑它。
const CLI_STEP = [
  '      - name: 按锁文件取签名 CLI',
  '        id: cli',
  '        env:',
  '          CLI_VERSION: ${{ needs.build.outputs.cli-version }}',
  '        run: |',
  '          node .github/scripts/locked-cli.mjs app/pnpm-lock.yaml "$CLI_VERSION" > "$RUNNER_TEMP/tauri-cli.tsv"',
  '          cli_root="$RUNNER_TEMP/tauri-cli/node_modules/@tauri-apps"',
  "          while IFS=$'\\t' read -r name integrity; do",
  '            tarball="$RUNNER_TEMP/$name-$CLI_VERSION.tgz"',
  '            curl --fail --silent --show-error --location --retry 3 --max-time 300 --output "$tarball" \\',
  '              "https://registry.npmjs.org/@tauri-apps/$name/-/$name-$CLI_VERSION.tgz" < /dev/null',
  '            actual="sha512-$(openssl dgst -sha512 -binary "$tarball" | base64 -w0)"',
  '            if [ "$actual" != "$integrity" ]; then',
  '              echo "::error::@tauri-apps/$name@$CLI_VERSION 的 integrity 是 $actual，锁文件里记的是 $integrity"',
  '              exit 1',
  '            fi',
  '            mkdir -p "$cli_root/$name"',
  '            tar -xzf "$tarball" -C "$cli_root/$name" --strip-components=1',
  '            echo "@tauri-apps/$name@$CLI_VERSION 与锁文件一致：$integrity"',
  '          done < "$RUNNER_TEMP/tauri-cli.tsv"',
  '          test -f "$cli_root/cli/tauri.js"',
  '          test -f "$cli_root/cli-linux-x64-gnu/cli.linux-x64-gnu.node"',
  '          echo "tauri=$cli_root/cli/tauri.js" >> "$GITHUB_OUTPUT"',
];

const SIGN_STEP = [
  '      - name: 给更新包签名',
  '        env:',
  '          TAURI_CLI: ${{ steps.cli.outputs.tauri }}',
  '          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.DEV_UPDATER_PRIVATE_KEY }}',
  '          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.DEV_UPDATER_KEY_PASSWORD }}',
  '        run: |',
  '          if [ -z "$TAURI_SIGNING_PRIVATE_KEY" ]; then',
  '            echo "::error::环境 updates-dev 里没有配置 DEV_UPDATER_PRIVATE_KEY" >&2',
  '            exit 1',
  '          fi',
  `          ${SIGN_COMMAND}`,
  '          test -s "$BOOTSTRAP.sig"',
];

// upload-cn 在「检查国内上传凭据」之后的每一步都带这个条件：没配凭据时不检出、不下载、不上传
const CONFIGURED = "if: steps.credentials.outputs.configured == 'true'";

// 上传到 cn 那一步的全文。凭据只进这一步、换成脚本认的名字；只传 cn；记录写在本作业自己的文件里，不交给清单；
// cn-run 在循环之后才写，任何一个包没传上时脚本已经以 1 退出，publish 拿到的是空的，照旧自己往 cn 传。
const UPLOAD_CN_STEP = [
  '      - name: 上传到 cn',
  '        id: upload',
  `        ${CONFIGURED}`,
  '        env:',
  '          OSS_CI_ACCESS_KEY_ID: ${{ secrets.OSS_CN_ACCESS_KEY_ID }}',
  '          OSS_CI_ACCESS_KEY_SECRET: ${{ secrets.OSS_CN_ACCESS_KEY_SECRET }}',
  '        run: |',
  '          for file in "$BOOTSTRAP" "$INSTALLER"; do',
  '            key="$(node .github/scripts/dev-distribution.mjs key "$VERSION" "$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT" "$(basename "$file")")"',
  '            bash .github/scripts/upload-to-mirrors.sh --only cn "$key" "$file" "$RUNNER_TEMP/cn-record.tsv"',
  '          done',
  '          echo "run=$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT" >> "$GITHUB_OUTPUT"',
];

// 仓库里的文件路径：.github 下的脚本与公钥，以及锁定签名 CLI 的锁文件。前面紧挨着 / 的是稀疏检出名单自己的写法，不算引用。
const PATH_REF = /(?<![\w./-])(\.github\/[\w./-]+|app\/pnpm-lock\.yaml)/g;
const MODULE_REF = /^\s*(?:import|export)\b[^;]*?\bfrom\s+["'](\.{1,2}\/[^"']+)["']/gm;
const SCRIPT_DIR_REF = /"\$script_dir\/([\w.-]+)"/g;

const resolveFrom = (path, spec) => new URL(spec, new URL(path, REPO_ROOT)).href.slice(REPO_ROOT.href.length);

/**
 * 这些行用到的仓库文件，连同它们再用到的：.mjs 的相对 import，shell 脚本按自己所在目录（$script_dir）调用的同伴。
 * 稀疏检出只取名单里的文件，漏一个就要等签名、上传做到一半才在 publish 里失败。
 */
function filesNeeded(code) {
  const pending = code.flatMap((line) => [...line.matchAll(PATH_REF)].map((match) => match[1]));
  const needed = new Set();
  while (pending.length > 0) {
    const path = pending.pop();
    if (needed.has(path)) continue;
    needed.add(path);
    const text = readFileSync(new URL(path, REPO_ROOT), 'utf8');
    if (path.endsWith('.mjs')) for (const match of text.matchAll(MODULE_REF)) pending.push(resolveFrom(path, match[1]));
    if (path.endsWith('.sh')) for (const match of text.matchAll(SCRIPT_DIR_REF)) pending.push(resolveFrom(path, `./${match[1]}`));
  }
  return [...needed].sort();
}

/** 步骤里 `sparse-checkout: |` 下面的名单，按行。 */
function sparseList(step) {
  const at = step.findIndex((line) => line.kind === 'structure' && line.key === 'sparse-checkout');
  assert.notEqual(at, -1, 'publish 的检出步骤不再用 sparse-checkout');
  assert.equal(step[at].valueKind, 'block', 'publish 的 sparse-checkout 应写成块纯量，一行一个路径');
  const list = [];
  for (const line of step.slice(at + 1)) {
    if (line.kind !== 'content') break;
    if (line.raw.trim() !== '') list.push(line.raw.trim());
  }
  return list;
}

describe('dev 构建工作流的契约', () => {
  it('结构行只用解析得了的写法：没有锚点、别名、合并键、带引号的键、续行和白名单以外的 flow 集合', () => {
    assert.deepEqual(PARSED.problems, [], '工作流用了逐行解析认不出的写法，下面按行比对的检查都可能被绕过去');
  });

  it('作业依次是 release-heads、build、upload-cn、publish、publish-hk，build 要等 release-heads 通过', () => {
    assert.deepEqual(
      jobNames(),
      ['release-heads', 'build', 'upload-cn', 'publish', 'publish-hk'],
      '作业变了：版本先后检查必须在编译之前单独成一个作业，不过时不用先白编',
    );
    assert.deepEqual(
      structure(jobLines('build'))
        .filter((line) => line.indent === 4 && line.key === 'needs')
        .map((line) => line.raw.trim()),
      ['needs: release-heads'],
      'build 不再等 release-heads：检查没过的提交照样编译、发布',
    );
  });

  // 它在编译之前读外网上的清单与 Release 附件，内容不可信；拿到机密或能写的令牌，读坏的清单就有机会借它做别的事。
  // 作业级的键整组钉住：多一个 environment 就拿得到环境里的私钥，多一个 if 或 continue-on-error 检查就不再挡构建。
  it('release-heads 跑在一次性托管机上，只读，不挂环境，不引用机密与仓库令牌，10 分钟时限', () => {
    assert.deepEqual(
      jobKeys('release-heads'),
      ['runs-on: ubuntu-24.04', 'timeout-minutes: 10', 'permissions:', 'outputs:', 'steps:'],
      'release-heads 的作业级键变了：它应跑在托管机 ubuntu-24.04 上、不挂环境、不带条件，时限 10 分钟',
    );
    assert.deepEqual(jobBlock('release-heads', 'permissions'), ['contents: read'], 'release-heads 的权限应只有 contents: read');
    assert.deepEqual(secretsIn(exposedOf(jobLines('release-heads'))), { names: [], other: [] }, 'release-heads 引用了机密');
    assert.deepEqual(tokenRefs(exposedOf(jobLines('release-heads'))), [], 'release-heads 引用了仓库令牌');
  });

  // 工作流级的 env、concurrency 这些地方引用的机密或令牌会进每一个作业，release-heads 也在内，按作业核对就落空了。
  it('作业之外哪里都不引用机密与仓库令牌', () => {
    const inJobs = new Set(jobNames().flatMap((name) => jobLines(name)));
    const outside = exposedOf(LINES.filter((line) => !inJobs.has(line)));
    assert.deepEqual(secretsIn(outside), { names: [], other: [] }, '作业之外引用了机密，release-heads 也会拿到');
    assert.deepEqual(tokenRefs(outside), [], '作业之外引用了仓库令牌，release-heads 也会拿到');
  });

  it('release-heads 依次读出各通道已发布的版本、过版本先后检查、查数据格式号，作业输出就是查过的版本号', () => {
    const own = steps('release-heads');
    assert.equal(own.length, 1 + RELEASE_HEADS_STEPS.length, 'release-heads 的步骤多了或少了：中间多一步就能改写 heads 文件或版本号');
    const checkout = stepKeys(own[0]);
    assert.ok(checkout[0].startsWith('uses: actions/checkout@'), 'release-heads 的第一步不再是检出');
    // 数据格式号的检查要读 HEAD 里的源码，并按 origin 取滚动预发布所指的提交，稀疏检出读不到
    assert.deepEqual(checkout.slice(1), ['with:'], 'release-heads 的检出多了键');
    assert.deepEqual(blockOf(own[0], 8, 'with'), ['persist-credentials: false'], 'release-heads 应完整检出、不留凭据');
    assert.deepEqual(
      own.slice(1).map(pinned),
      RELEASE_HEADS_STEPS,
      'release-heads 的步骤变了：算版本号、read、gate dev release、data-format dev release 应依次无条件执行',
    );
    assert.deepEqual(
      jobBlock('release-heads', 'outputs'),
      ['version: ${{ steps.version.outputs.version }}'],
      'release-heads 交给 build 的版本号应就是交给版本先后检查的那一个',
    );
  });

  it('build 把 release-heads 交出的版本号原样写进 tauri.conf.json，不再自己算', () => {
    const build = steps('build');
    assert.deepEqual(
      pinned(build[stepIndex('build', '写入 dev 版本号')]),
      WRITE_VERSION_STEP,
      '「写入 dev 版本号」变了：它应原样写入 needs.release-heads.outputs.version，不再自己算版本号',
    );
    assert.deepEqual(
      CODE.filter((line) => line.includes('$GITHUB_RUN_NUMBER')),
      [RELEASE_HEADS_STEPS[0][4].trim()],
      '版本号只该在 release-heads 里算一次：别处再算，检查过的和发出去的可能不是同一个',
    );
    const versions = structure(jobLines('build'))
      .filter((line) => line.key === 'VERSION')
      .map((line) => line.raw.trim());
    assert.deepEqual(
      versions.filter((line) => line !== 'VERSION: ${{ steps.version.outputs.version }}'),
      ['VERSION: ${{ needs.release-heads.outputs.version }}'],
      'build 里的 VERSION 应只从 release-heads 取一次，其余都取「写入 dev 版本号」写下的那个',
    );
    const outputs = jobBlock('build', 'outputs');
    assert.ok(outputs.includes('version: ${{ steps.version.outputs.version }}'), 'build 交给 publish 的版本号不再是写进安装包的那一个');
    assert.ok(outputs.includes('cli-version: ${{ steps.version.outputs.cli-version }}'), 'build 不再把锁定的 CLI 版本交给 publish');
  });

  // 换成发布公钥，所有 dev 客户端拒收 dev 钥签的包；漏了开关，detect_support 判为不支持，dev 包收不到任何更新。
  // 整行比对，多带一项（比如 allowDowngrades）也要挂。注入必须紧挨着打包、无条件执行，打包那一步在 pnpm tauri build
  // 之前也不做别的：中间多一步或多一行（比如把 tauri.conf.json 还原回去），打出的包同样不带公钥。
  it('build 紧挨着打包无条件注入开发版公钥并打开 requireSignedVersion', () => {
    assert.deepEqual(CODE.filter((line) => line.includes('conf.plugins')), [PUBKEY_LINE], '注入更新插件配置的那一行变了，或不止一处');
    const build = steps('build');
    const inject = build.findIndex((step) => codeOf(step).includes(PUBKEY_LINE));
    assert.notEqual(inject, -1, '更新插件的配置不再写在 build 作业里');
    assert.deepEqual(stepText(build[inject]), INJECT_STEP, '注入公钥那一步多了键、改了脚本，注入可能不再执行');
    const bundle = build.findIndex((step) => codeOf(step).includes(BUILD_COMMAND));
    assert.notEqual(bundle, -1, `build 作业里找不到打包启动器的那一步：${BUILD_COMMAND}`);
    assert.equal(bundle, inject + 1, '注入公钥与打包之间夹了别的步骤，或排到了打包之后');
    const script = codeOf(build[bundle].filter((line) => line.kind === 'content'));
    assert.deepEqual(
      script.slice(0, 3),
      ['bundle_dir="$CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/bundle"', 'rm -rf "$bundle_dir"', BUILD_COMMAND],
      '打包那一步在 pnpm tauri build 之前多做了别的事',
    );
  });

  // 少了内嵌凭据的包登录不了、搜不了 CurseForge；带着 CPU 抖动熵源的包，aws-lc 初始化它失败时直接终止进程。
  // 这几条都只有在打出来的 exe 里找才知道，查不到（或查到了不该有的）就不能发给 dev 用户；这一步多一个 if 或
  // continue-on-error 就等于没查。
  it('打包后在 Petalfall.exe 里核对内嵌的凭据，并核对没有编进 CPU 抖动熵源', () => {
    const bundle = steps('build').find((step) => codeOf(step).includes(BUILD_COMMAND));
    assert.notEqual(bundle, undefined, `build 作业里找不到打包启动器的那一步：${BUILD_COMMAND}`);
    assert.deepEqual(stepKeys(bundle), ['name: 构建 Windows 安装包', 'id: bundle', 'working-directory: app', 'env:', 'run: |']);
    assert.deepEqual(blockOf(bundle, 8, 'env'), ['VERSION: ${{ steps.version.outputs.version }}']);
    assert.deepEqual(
      codeOf(bundle.filter((line) => line.kind === 'content')),
      BUNDLE_SCRIPT,
      '打包那一步的脚本变了，exe 里的核对可能不再执行',
    );
  });

  // 换钥时 CI 里打包、签名、验签都对得上，坏在已装旧版的用户那头：他们只认编进旧包的公钥，换钥之后发的更新全被拒收。
  // 所以把密钥号钉死，换钥必须连这里一起改；顺带挡住把发布公钥误当开发版公钥提交。密钥号写成验签脚本输出的样子
  // （小写十六进制，和 minisign 注释里那串字节序相反）。公钥文件是 minisign 公钥框整个再做一次 base64，
  // 框的第二行解出 42 字节：算法 Ed、8 字节密钥号、32 字节 Ed25519 公钥。
  it('开发版公钥是记录在案的那把，不是发布钥', () => {
    const keyNumber = (name) => {
      const box = Buffer.from(readFileSync(new URL(`.github/${name}`, REPO_ROOT), 'utf8').trim(), 'base64').toString('utf8').split('\n');
      const key = Buffer.from(box[1].trim(), 'base64');
      assert.equal(key.length, 42, `${name} 解出的公钥不是 42 字节`);
      assert.equal(key.subarray(0, 2).toString(), 'Ed', `${name} 的算法不是 Ed`);
      return key.subarray(2, 10).toString('hex');
    };
    assert.equal(keyNumber('dev-updater.pub'), 'a85c28a4343f0962', '开发版公钥换了：已装的 dev 包只认原来那把，换钥之后发的更新全被拒收');
    assert.notEqual(keyNumber('dev-updater.pub'), keyNumber('release-updater.pub'), '开发版公钥与发布公钥是同一把');
  });

  // 多一个 if 或 continue-on-error 就等于没查：载荷头写错、少编一把公钥的包照样上传、发布。
  it('「构建首装引导程序」之后紧接着无条件核对载荷头与内嵌的更新公钥', () => {
    const build = steps('build');
    const bootstrap = stepIndex('build', '构建首装引导程序');
    const payload = stepIndex('build', '核对载荷头');
    const pubkeys = stepIndex('build', '核对内嵌的更新公钥');
    assert.equal(payload, bootstrap + 1, '「核对载荷头」应紧接在「构建首装引导程序」之后');
    assert.equal(pubkeys, payload + 1, '「核对内嵌的更新公钥」应紧接在「核对载荷头」之后');
    for (const index of [payload, pubkeys]) {
      const keys = stepKeys(build[index]);
      assert.deepEqual(
        keys.filter((key) => /^(?:if|continue-on-error)\s*:/i.test(key)),
        [],
        `${keys[0]} 带了条件或 continue-on-error，核对不过也不挡发布`,
      );
    }
    assert.deepEqual(pinned(build[payload]), PAYLOAD_STEP, '「核对载荷头」变了，核对可能不再执行或不再全查四项');
    assert.deepEqual(pinned(build[pubkeys]), PUBKEY_CHECK_STEP, '「核对内嵌的更新公钥」变了，核对可能不再执行或漏查一把');
  });

  // 版本序、数据格式号与安装程序判定表的规则测试都在 petalfall-release 里，CI 上只跑这两个包的 Rust 测试。
  it('「构建首装引导程序」跑的 Rust 测试带上 petalfall-release', () => {
    assert.deepEqual(
      CODE.filter((line) => line.startsWith('cargo test')),
      [CARGO_TEST],
      'CI 上的 Rust 测试变了：应恰好一处，跑 petalfall-setup-payload 与 petalfall-release',
    );
    assert.ok(
      codeOf(steps('build')[stepIndex('build', '构建首装引导程序')]).includes(CARGO_TEST),
      'Rust 测试不再在「构建首装引导程序」里跑，载荷头与规则没测过就核对、发布',
    );
  });

  // 持钥的作业里跑包管理器，取到的是 registry 当下给的那一份，还会跑安装脚本，私钥就在同一台机器上。
  it('publish 里不跑包管理器，签名 CLI 经 locked-cli.mjs 按锁文件取、核对 integrity 之后才拿私钥去跑', () => {
    assert.deepEqual(
      job('publish').filter((line) => /\b(?:npm|npx|pnpm|yarn)\s/.test(line)),
      [],
      'publish 里又跑了包管理器（比如 npm exec）',
    );
    const publish = steps('publish');
    const cli = stepIndex('publish', '按锁文件取签名 CLI');
    const sign = stepIndex('publish', '给更新包签名');
    assert.ok(cli < sign, '「按锁文件取签名 CLI」应排在「给更新包签名」之前');
    assert.deepEqual(pinned(publish[cli]), CLI_STEP, '「按锁文件取签名 CLI」变了，integrity 可能不再核对');
    assert.deepEqual(pinned(publish[sign]), SIGN_STEP, '「给更新包签名」变了：应只用上一步核对过的 CLI 签名，并把版本号写进可信注释');
    assert.deepEqual(
      job('publish').filter((line) => line.includes('signer sign')),
      [SIGN_COMMAND],
      'publish 里应恰有一条签名命令，用的是按锁文件取来的 CLI',
    );
  });

  // 取 CLI 那一步要从外网下载、解包；私钥只给真正签名的那一步，下载与解包的过程碰不到它。
  it('dev 签名私钥只进「给更新包签名」那一步', () => {
    const publish = steps('publish');
    const sign = stepIndex('publish', '给更新包签名');
    const holders = publish.flatMap((step, index) =>
      secretsIn(exposedOf(step)).names.some((name) => name.startsWith('DEV_UPDATER_')) ? [index] : [],
    );
    assert.deepEqual(holders, [sign], 'dev 签名私钥或口令进了签名以外的步骤');
    const elsewhere = exposedOf(LINES.filter((line) => !publish[sign].includes(line)));
    assert.deepEqual(
      secretsIn(elsewhere).names.filter((name) => name.startsWith('DEV_UPDATER_')),
      [],
      'dev 签名私钥或口令出现在签名那一步之外（作业级 env、别的作业）',
    );
    assert.deepEqual(secretsIn(exposedOf(publish[stepIndex('publish', '按锁文件取签名 CLI')])), { names: [], other: [] });
  });

  it('publish 只检出用得到的文件，一个不少：锁文件、读锁文件的脚本，以及各步用到的脚本与公钥', () => {
    const publish = steps('publish');
    const checkout = publish.findIndex((step) => stepKeys(step)[0].startsWith('uses: actions/checkout@'));
    assert.notEqual(checkout, -1, 'publish 不再检出仓库');
    assert.deepEqual(
      blockOf(publish[checkout], 8, 'with'),
      ['persist-credentials: false', 'sparse-checkout: |', 'sparse-checkout-cone-mode: false'],
      'publish 的检出应是非 cone 模式的稀疏检出、不留凭据：cone 模式只能按目录取',
    );
    const needed = filesNeeded(publish.filter((_, index) => index !== checkout).flatMap((step) => codeOf(step)));
    assert.ok(needed.includes('app/pnpm-lock.yaml'), 'publish 不再按锁文件取签名 CLI');
    assert.ok(needed.includes('.github/scripts/locked-cli.mjs'), 'publish 不再经 locked-cli.mjs 读锁文件');
    assert.deepEqual(
      sparseList(publish[checkout]),
      needed.map((path) => `/${path}`),
      'publish 的稀疏检出名单与各步用到的文件对不上：少了的会让 publish 在签名、上传做到一半时失败，多了的白放在持钥的机器上',
    );
  });

  // dev 上 cn 传不上只降级，降级靠上传记录：上传那一步不写记录时 cn 一失败整个 publish 就停；build、validate 读的
  // 不是同一份时，清单可能列出没传上的桶，或者漏掉传上了的
  it('publish 上传时写上传记录，build 与 validate 读的是同一份', () => {
    const publish = job('publish');
    const record = '"$RUNNER_TEMP/mirror-record.tsv"';
    assert.deepEqual(
      // 稀疏检出名单里也有这个文件名，只看调用它的行
      publish.filter((line) => line.includes('bash .github/scripts/upload-to-mirrors.sh')),
      [`bash .github/scripts/upload-to-mirrors.sh "\${cn[@]}" "$key" "$file" ${record}`],
      'publish 上传时不再写上传记录',
    );
    const manifestCalls = publish.filter((line) => /dev-distribution\.mjs (?:build|validate) /.test(line));
    assert.equal(manifestCalls.length, 2, `publish 里生成、校验清单的地方变了：${manifestCalls}`);
    assert.ok(manifestCalls[0].endsWith(`"$GITHUB_SHA" ${record} "$manifest"`), `build 没读上传记录：${manifestCalls[0]}`);
    assert.ok(manifestCalls[1].endsWith(`"$TAG" ${record}`), `validate 没读上传记录：${manifestCalls[1]}`);
  });

  // 内网自托管机上跑的是仓库里的代码：多一个 pull_request 触发，任何 fork 都能在这台机器上跑代码、拿 upload-cn 的凭据
  it('只由推送 dev 触发', () => {
    assert.deepEqual(blockOf(LINES, 0, 'on'), ['push:', 'branches: [dev]'], '触发条件变了：自托管机上的作业只能由推送 dev 触发');
  });

  // 托管机往杭州跨境上传常挂住，cn 改由国内的自托管机传。作业级的键整组钉住：多一个 environment 或 concurrency、
  // 少一个 permissions，交出的凭据、会不会被别的运行顶掉、令牌能做什么就都变了。
  it('upload-cn 跑在国内的自托管机上，挂单独的环境 updates-dev-cn，只读权限，不进并发组，只交出 cn-run', () => {
    assert.deepEqual(jobKeys('upload-cn'), [
      'needs: build',
      'runs-on: [self-hosted, Linux, X64, petalfall]',
      'environment: updates-dev-cn',
      'timeout-minutes: 30',
      'permissions:',
      'outputs:',
      'env:',
      'steps:',
    ]);
    assert.deepEqual(jobBlock('upload-cn', 'permissions'), ['contents: read']);
    assert.deepEqual(jobBlock('upload-cn', 'outputs'), ['cn-run: ${{ steps.upload.outputs.run }}']);
    // 传的必须是 publish 签名、读回的那两份字节，对象键也按同一个版本号算
    assert.deepEqual(
      jobBlock('upload-cn', 'env'),
      jobBlock('publish', 'env').filter((line) => !line.startsWith('HK_MANIFEST_URL:')),
      'upload-cn 的 VERSION、INSTALLER、BOOTSTRAP 与 publish 的不同，读回时对不上',
    );
  });

  // 自托管机跨任务常驻：签名私钥、R2 与 hk 的凭据、香港推送钥任何一样进了这台机器上的作业，机器被污染时就跟着泄露
  it('自托管机上的作业只拿得到要编进包里的 CurseForge key 与 cn 的那一对凭据，cn 的凭据只进检查与上传两步', () => {
    const selfHosted = jobNames().filter((name) => jobKeys(name).includes('runs-on: [self-hosted, Linux, X64, petalfall]'));
    assert.deepEqual(selfHosted, ['build', 'upload-cn']);
    assert.deepEqual(secretsIn(exposedOf(jobLines('build'))), { names: ['PETALFALL_CURSEFORGE_API_KEY'], other: [] });
    assert.deepEqual(secretsIn(exposedOf(jobLines('upload-cn'))), { names: ['OSS_CN_ACCESS_KEY_ID', 'OSS_CN_ACCESS_KEY_SECRET'], other: [] });
    assert.deepEqual(tokenRefs(exposedOf(jobLines('upload-cn'))), [], 'upload-cn 引用了仓库令牌');
    assert.deepEqual(codeOf(jobLines('upload-cn')).filter((line) => /\bvars\s*\./i.test(line)), [], 'upload-cn 读了仓库或环境变量，比如 R2 的账户 ID');
    const own = steps('upload-cn');
    const holders = own.flatMap((step, index) => (secretsIn(exposedOf(step)).names.length > 0 ? [index] : []));
    assert.deepEqual(holders, [stepIndex('upload-cn', '检查国内上传凭据'), stepIndex('upload-cn', '上传到 cn')], 'cn 的凭据进了检查与上传以外的步骤');
    const elsewhere = exposedOf(LINES.filter((line) => !jobLines('upload-cn').includes(line)));
    assert.deepEqual(secretsIn(elsewhere).names.filter((name) => name.startsWith('OSS_CN_')), [], 'cn 的国内凭据出现在 upload-cn 之外');
  });

  it('upload-cn 先查凭据，后面每一步都只在配了凭据时跑；完整检出，只往 cn 传，cn-run 写在传完之后', () => {
    const own = steps('upload-cn');
    assert.deepEqual(stepKeys(own[0]), ['name: 检查国内上传凭据', 'id: credentials', 'env:', 'run: |']);
    for (const step of own.slice(1)) {
      assert.ok(stepKeys(step).includes(CONFIGURED), `${stepKeys(step)[0]} 没有限定只在配了凭据时跑`);
    }
    const checkout = own.findIndex((step) => stepKeys(step)[0].startsWith('uses: actions/checkout@'));
    assert.equal(checkout, 1, 'upload-cn 第二步不再是检出');
    // 稀疏检出的设置会留在这台机器跨作业保留的工作区里
    assert.deepEqual(blockOf(own[checkout], 8, 'with'), ['persist-credentials: false']);
    assert.equal(stepIndex('upload-cn', '上传到 cn'), own.length - 1, '「上传到 cn」之后还有别的步骤');
    assert.deepEqual(pinned(own.at(-1)), UPLOAD_CN_STEP, '「上传到 cn」变了：应只往 cn 传、按通道规矩写本作业自己的记录，传完才交出 cn-run');
  });

  // 写了状态函数就不再默认要求前面的作业都成功：漏了 needs.build.result，build 失败时也会发布；
  // publish-hk 不写条件时默认要求整条依赖链都成功，upload-cn 失败而 publish 已重建预发布时它会被跳过，香港停在旧版
  it('upload-cn 失败时 publish 与 publish-hk 照样跑，build 没成功或运行被取消时都不跑', () => {
    const conditions = (name) => jobKeys(name).filter((key) => /^(?:needs|if|continue-on-error)\s*:/.test(key));
    assert.deepEqual(conditions('publish'), ['needs: [build, upload-cn]', "if: ${{ !cancelled() && needs.build.result == 'success' }}"]);
    assert.deepEqual(conditions('publish-hk'), [
      'needs: [build, publish]',
      "if: ${{ !cancelled() && needs.build.result == 'success' && needs.publish.result == 'success' }}",
    ]);
  });

  // 取舍见 docs/architecture.md 的并发一节：build 被新推送取消；publish、publish-hk 各自串行、不取消、不共用一个组；
  // upload-cn 只写本次运行独有的目录，进了组反而会被后排进来的作业顶掉
  it('并发组只设在作业上：build 可取消，publish 与 publish-hk 各一组不取消，upload-cn 与 release-heads 不进组', () => {
    assert.ok(!structure(LINES).some((line) => line.indent === 0 && line.key === 'concurrency'), '设了工作流级的并发组');
    const groups = Object.fromEntries(jobNames().map((name) => [name, jobKeys(name).includes('concurrency:') ? jobBlock(name, 'concurrency') : null]));
    assert.deepEqual(groups, {
      'release-heads': null,
      build: ['group: dev-build', 'cancel-in-progress: true'],
      'upload-cn': null,
      publish: ['group: dev-publish', 'cancel-in-progress: false'],
      'publish-hk': ['group: dev-publish-hk', 'cancel-in-progress: false'],
    });
  });
});

// 交接 cn 的几段脚本按 GitHub 跑 shell: bash 的方式（bash --noprofile --norc -eo pipefail）真跑一遍，看各种输入下实际做了什么。
// beta-release.yml 的同名步骤与这里逐字相同，由那边的契约核对，这里测过的就是两边的行为。
describe('upload-cn 与 publish 交接 cn 的脚本', () => {
  const root = mkdtempSync(join(tmpdir(), 'dev-build-contract-'));
  after(() => rmSync(root, { recursive: true, force: true }));
  let nextCase = 0;
  const VERSION = `1.${randomInt(0, 30)}.0-dev.${randomInt(1, 100_000)}`;
  const RUN_ID = String(randomInt(1, 2 ** 40));
  const ATTEMPT = randomInt(1, 6);
  const RUN = `${RUN_ID}-${ATTEMPT}`;
  const BOOTSTRAP_NAME = `Petalfall-Installer-${VERSION}-x64.exe`;
  const INSTALLER_NAME = `Petalfall_${VERSION}_x64-setup.exe`;
  const keyOf = (name) => `dev/${VERSION}-${RUN}/${name}`;
  // Windows 上 bash 的 basename 不认反斜杠
  const posix = (path) => path.replaceAll('\\', '/');

  // 工作区里放真的 dev-distribution.mjs 和一个假的 upload-to-mirrors.sh：假脚本把参数一行一个记进 $FAKE_LOG，
  // 每次以 --- 结尾，第 $FAKE_FAIL_AT 次调用以 1 退出
  function workspace() {
    const dir = join(root, String(nextCase++));
    mkdirSync(join(dir, '.github', 'scripts'), { recursive: true });
    copyFileSync(new URL('./dev-distribution.mjs', import.meta.url), join(dir, '.github', 'scripts', 'dev-distribution.mjs'));
    writeFileSync(
      join(dir, '.github', 'scripts', 'upload-to-mirrors.sh'),
      [
        '{ printf "%s\\n" "$@"; echo ---; } >> "$FAKE_LOG"',
        'if [ "$(grep -c "^---$" "$FAKE_LOG")" = "${FAKE_FAIL_AT:-0}" ]; then exit 1; fi',
        '',
      ].join('\n'),
    );
    mkdirSync(join(dir, 'temp'));
    const env = {};
    let path = '';
    for (const [name, value] of Object.entries(process.env)) {
      if (name.toUpperCase() === 'PATH') path = value;
      else if (!/^(?:GITHUB_|RUNNER_|OSS_|CN_)/i.test(name)) env[name] = value;
    }
    Object.assign(env, {
      PATH: path,
      FAKE_LOG: posix(join(dir, 'calls.log')),
      GITHUB_OUTPUT: posix(join(dir, 'output')),
      GITHUB_STEP_SUMMARY: posix(join(dir, 'summary.md')),
      RUNNER_TEMP: posix(join(dir, 'temp')),
      GITHUB_RUN_ID: RUN_ID,
      GITHUB_RUN_ATTEMPT: String(ATTEMPT),
      VERSION,
      BOOTSTRAP: posix(join(dir, 'dist', BOOTSTRAP_NAME)),
      INSTALLER: posix(join(dir, 'dist', INSTALLER_NAME)),
    });
    const read = (name) => (existsSync(join(dir, name)) ? readFileSync(join(dir, name), 'utf8') : '');
    const calls = () =>
      read('calls.log')
        .split('---\n')
        .filter((call) => call !== '')
        .map((call) => call.split('\n').slice(0, -1));
    return { dir, env, read, calls };
  }

  function runStep(step, { dir, env }, extra = {}) {
    const file = join(dir, 'step.sh');
    writeFileSync(file, scriptOf(step));
    return spawnSync('bash', ['--noprofile', '--norc', '-eo', 'pipefail', file], { encoding: 'utf8', cwd: dir, env: { ...env, ...extra } });
  }

  const credentialStep = () => steps('upload-cn')[stepIndex('upload-cn', '检查国内上传凭据')];
  const uploadCnStep = () => steps('upload-cn')[stepIndex('upload-cn', '上传到 cn')];
  const publishUploadStep = () => steps('publish')[stepIndex('publish', '上传到三个存储桶')];
  const ID = `LTAI${randomBytes(8).toString('hex')}`;
  const SECRET = randomBytes(15).toString('hex');

  it('凭据两个都没配（为空或不存在）时只打一条警告、写进作业摘要，成功结束，交出 configured=false', () => {
    for (const extra of [{ OSS_CN_ACCESS_KEY_ID: '', OSS_CN_ACCESS_KEY_SECRET: '' }, {}]) {
      const ws = workspace();
      const result = runStep(credentialStep(), ws, extra);
      assert.equal(result.status, 0, result.stdout + result.stderr);
      assert.equal(
        result.stdout,
        '::warning::作业所在的环境里没有配置 OSS_CN_ACCESS_KEY_ID 与 OSS_CN_ACCESS_KEY_SECRET，cn 照旧由托管机上的发布作业上传\n',
      );
      assert.equal(ws.read('output'), 'configured=false\n');
      assert.match(ws.read('summary.md'), /^\*\*cn 由托管机上传\*\*：作业所在的环境里没有配置国内上传凭据 OSS_CN_ACCESS_KEY_ID、OSS_CN_ACCESS_KEY_SECRET/);
    }
  });

  it('两个都配了时交出 configured=true，不打警告、不写摘要；只配了一个时失败，不交出 configured', () => {
    const ws = workspace();
    const result = runStep(credentialStep(), ws, { OSS_CN_ACCESS_KEY_ID: ID, OSS_CN_ACCESS_KEY_SECRET: SECRET });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(result.stdout, '');
    assert.equal(ws.read('output'), 'configured=true\n');
    assert.equal(ws.read('summary.md'), '');
    for (const extra of [{ OSS_CN_ACCESS_KEY_ID: ID, OSS_CN_ACCESS_KEY_SECRET: '' }, { OSS_CN_ACCESS_KEY_ID: '', OSS_CN_ACCESS_KEY_SECRET: SECRET }]) {
      const half = workspace();
      const failed = runStep(credentialStep(), half, extra);
      assert.equal(failed.status, 1, JSON.stringify(extra));
      assert.equal(failed.stdout, '::error::作业所在的环境里 OSS_CN_ACCESS_KEY_ID 与 OSS_CN_ACCESS_KEY_SECRET 只配了一个\n');
      assert.equal(half.read('output'), '');
    }
  });

  it('上传到 cn：两个包依次只传 cn、写本作业自己的记录，都传完才交出这一次的运行标识', () => {
    const ws = workspace();
    const result = runStep(uploadCnStep(), ws);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    const record = `${ws.env.RUNNER_TEMP}/cn-record.tsv`;
    assert.deepEqual(ws.calls(), [
      ['--only', 'cn', keyOf(BOOTSTRAP_NAME), ws.env.BOOTSTRAP, record],
      ['--only', 'cn', keyOf(INSTALLER_NAME), ws.env.INSTALLER, record],
    ]);
    assert.equal(ws.read('output'), `run=${RUN}\n`);

    for (const failAt of ['1', '2']) {
      const failing = workspace();
      const failed = runStep(uploadCnStep(), failing, { FAKE_FAIL_AT: failAt });
      assert.equal(failed.status, 1, failAt);
      assert.equal(failing.calls().length, Number(failAt));
      assert.equal(failing.read('output'), '', `第 ${failAt} 个包没传上时交出了 cn-run，publish 会只读回、不再自己传`);
    }
  });

  it('publish 上传：cn-run 等于这一次的运行标识时只读回 cn，其余情形一律照旧自己往 cn 传', () => {
    const record = (ws) => `${ws.env.RUNNER_TEMP}/mirror-record.tsv`;
    const plain = (ws) => [
      [keyOf(BOOTSTRAP_NAME), ws.env.BOOTSTRAP, record(ws)],
      [keyOf(INSTALLER_NAME), ws.env.INSTALLER, record(ws)],
    ];

    const current = workspace();
    const readback = runStep(publishUploadStep(), current, { CN_RUN: RUN, CN_RESULT: 'success' });
    assert.equal(readback.status, 0, readback.stdout + readback.stderr);
    assert.deepEqual(current.calls(), plain(current).map((args) => ['--readback-only', 'cn', ...args]));
    assert.equal(readback.stdout, 'cn 已由 upload-cn 从国内传过，这里只按公网地址读回核对\n');

    const otherRuns = [`${RUN_ID}-${ATTEMPT + 1}`, `${Number(RUN_ID) + 1}-${ATTEMPT}`, `${RUN_ID}-${ATTEMPT}0`];
    const cases = [
      { CN_RUN: '', CN_RESULT: 'success', stdout: /^upload-cn 所在的环境里没有配置国内上传凭据，cn 照旧从托管机上传\n$/ },
      ...['failure', 'cancelled', 'skipped'].map((result) => ({ CN_RUN: '', CN_RESULT: result, stdout: new RegExp(`^::warning::upload-cn 的结果是 ${result}，cn 改从托管机上传\\n$`) })),
      ...otherRuns.map((run) => ({
        CN_RUN: run,
        CN_RESULT: 'success',
        stdout: new RegExp(`^::warning::upload-cn 交来的是运行标识 ${run} 的结果，这一次是 ${RUN}：它没跟着重跑，传的是另一个目录，cn 改从托管机上传。`),
      })),
    ];
    for (const { stdout, ...extra } of cases) {
      const ws = workspace();
      const result = runStep(publishUploadStep(), ws, extra);
      assert.equal(result.status, 0, `${JSON.stringify(extra)}：${result.stdout}${result.stderr}`);
      assert.deepEqual(ws.calls(), plain(ws), JSON.stringify(extra));
      assert.match(result.stdout, stdout, JSON.stringify(extra));
    }
  });
});
