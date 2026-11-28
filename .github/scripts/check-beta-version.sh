#!/usr/bin/env bash
# 测试版的版本号不倒退。插件只装比自己新的版本：香港 beta 清单是客户端唯一会问的地方，对照它，还没有时 404，跳过；
# 再对照所有 v* tag：香港清单被撤掉止血之后，这一条照样挡得住倒退，也挡得住同号重发。
# stage 在传包之前跑它，upload-cn 在往 cn 传之前也跑：beta/ 下的对象禁止覆盖写、有锁，会在这里失败的运行不该先往桶里
# 留下删不掉的目录。两处共用这一份，判法不会各写各的。
# 用法：bash check-beta-version.sh <版本> <香港 beta 清单地址>
# tag 从 $GITHUB_SERVER_URL/$GITHUB_REPOSITORY 匿名列出。
set -euo pipefail

if [ "$#" -ne 2 ] || [ -z "$1" ] || [ -z "$2" ]; then
  echo "用法：check-beta-version.sh <版本> <香港 beta 清单地址>" >&2
  exit 2
fi
version="$1"
manifest_url="$2"
tag="v$version"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

status="$(curl --silent --show-error --retry 3 --max-time 60 --output "$work/previous-hk.json" --write-out '%{http_code}' "$manifest_url")"
case "$status" in
  200)
    previous="$(node -e "
      const version = JSON.parse(require('fs').readFileSync(process.argv[1], 'utf8')).version;
      if (typeof version !== 'string') throw new Error('香港 beta 清单里没有字符串形式的 version');
      process.stdout.write(version);
    " "$work/previous-hk.json")"
    order="$(node "$script_dir/dev-distribution.mjs" compare "$version" "$previous")"
    if [ "$order" != 1 ]; then
      echo "::error::版本号 $version 不比香港 beta 清单上已发布的 $previous 新，beta 包不会更新过去"
      exit 1
    fi
    ;;
  404) echo "香港机上还没有 beta 清单，跳过" ;;
  *)
    echo "::error::读香港清单 $manifest_url 返回 HTTP $status"
    exit 1
    ;;
esac
git ls-remote --tags --refs "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY.git" | cut -f2 | sed 's#^refs/tags/##' > "$work/tags.txt"
while IFS= read -r existing; do
  case "$existing" in
    "$tag")
      echo "::error::tag $tag 已经存在：这个版本发布过了"
      exit 1
      ;;
    v*)
      order="$(node "$script_dir/dev-distribution.mjs" compare "$version" "${existing#v}" < /dev/null)"
      if [ "$order" != 1 ]; then
        echo "::error::版本号 $version 不比已有的 tag $existing 新"
        exit 1
      fi
      ;;
  esac
done < "$work/tags.txt"
echo "版本号 $version 比香港 beta 清单和已有的 v* tag 都新"
