#!/usr/bin/env bash
# 把一个文件按同一个对象键传到三个存储桶：杭州 OSS（cn）、香港 OSS（hk）、R2（global），每传完一个桶就不带凭据、
# 按客户端会用的公网地址读回，大小和 sha256 与本地一致才算传上：公共读没开、自定义域名配错时上传照样成功，只有读回看得出来。
# 桶、端点、区域与公网主机都从 dev-distribution.mjs 的同一张表来，读回和清单用的是同一套地址。
# 用法：bash upload-to-mirrors.sh [--only <源名>]... [--readback-only <源名>]... <对象键> <本地文件> [上传记录]
# 不给上传记录时处理的桶都必须传上，哪个失败都退出 1（分发试跑就这样用，要看清每个桶）。给了上传记录时，每个桶的结果追加
# 一行「源名<tab>ok|failed<tab>对象键」，dev-distribution.mjs 的 build 只把记为 ok 的桶写进清单；本通道允许缺的桶
# （dev-distribution.mjs optional-mirrors）失败时只打警告、写进作业摘要、记为 failed，接着传后面的桶，同一份记录里
# 它失败过之后，后面的文件也不再传它；其余的桶失败照样退出 1。
# --only 只处理点名的桶，可以给多次：国内的自托管机只传 cn。这时只要求点名的桶上传要用的那组凭据，不碰别的变量；
# 记录里也只有点名的桶，不能交给 build 生成清单，那里要三个桶各一行。
# --readback-only 点名的桶已由别处传过，可以给多次，必须在处理的桶里：不上传、不用它的凭据，只按公网地址读回，一致才记 ok；
# 对不上和上传失败一样，按这个桶在本通道的规矩处理，同一份记录里失败过之后，后面的文件也不再读回它。
# 凭据只从调用它的那一步的环境变量来，缺任何一个都直接失败；不给 --only 时下面五个都要：
#   OSS_CI_ACCESS_KEY_ID、OSS_CI_ACCESS_KEY_SECRET（RAM 子用户，只有 oss:PutObject，限两个桶里本通道的前缀，每个通道一个子用户；
#   国内机只传 cn，用的是另一个只放行 cn 里本通道前缀的子用户，工作流把它的凭据以同样的名字交进来）
#   R2_CI_ACCESS_KEY_ID、R2_CI_SECRET_ACCESS_KEY（R2 的 S3 凭据，只授 petalfall-dl 一个桶）
#   R2_ACCOUNT_ID（拼 R2 的 S3 端点）
set -euo pipefail

usage="用法：upload-to-mirrors.sh [--only <源名>]... [--readback-only <源名>]... <对象键> <本地文件> [上传记录]"
only=()
readback_only=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --only | --readback-only)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "$usage" >&2
        exit 2
      fi
      if [ "$1" = --only ]; then
        only+=("$2")
      else
        readback_only+=("$2")
      fi
      shift 2
      ;;
    -*)
      echo "$usage" >&2
      exit 2
      ;;
    *) break ;;
  esac
done
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ] || { [ "$#" -eq 3 ] && [ -z "$3" ]; }; then
  echo "$usage" >&2
  exit 2
fi
key="$1"
file="$2"
record="${3:-}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# listed <值> <元素>...：值是不是其中之一
listed() {
  local wanted="$1" item
  shift
  for item in "$@"; do
    if [ "$item" = "$wanted" ]; then
      return 0
    fi
  done
  return 1
}

require() {
  local var
  for var in "$@"; do
    if [ -z "${!var:-}" ]; then
      echo "::error::作业所在的环境里没有配置 $var" >&2
      exit 1
    fi
  done
}

if [ "${#only[@]}" -eq 0 ]; then
  require OSS_CI_ACCESS_KEY_ID OSS_CI_ACCESS_KEY_SECRET R2_CI_ACCESS_KEY_ID R2_CI_SECRET_ACCESS_KEY R2_ACCOUNT_ID
fi
test -f "$file"
# 对象键必须是 <通道>/<目录>/<文件名>，通道只认 dev、beta：OSS 的 RAM 策略只放行本通道的前缀，R2 的凭据却能写整个桶，
# 写偏了就落在桶的规则之外，dev/ 的 30 天过期、beta/ 的禁止覆盖写与锁都管不到它。
# mirror-urls 用的就是这条校验；它给出的公网地址留给下面每个桶传完后读回。
public_urls="$(node "$script_dir/dev-distribution.mjs" mirror-urls "$key")"

# 点名的桶在这里就核对完：名字拼错、重复、只读回的桶不在处理之列，都说明调用方写错了，不能传了一半才发现
mapfile -t names < <(cut -f1 <<< "$public_urls")
checked=()
for name in "${only[@]}"; do
  if ! listed "$name" "${names[@]}" || listed "$name" "${checked[@]}"; then
    echo "::error::--only 点名的 $name 不是桶名或重复了，桶名只有 ${names[*]}" >&2
    exit 2
  fi
  checked+=("$name")
done
processed=("${names[@]}")
if [ "${#only[@]}" -gt 0 ]; then
  processed=("${only[@]}")
fi
checked=()
for name in "${readback_only[@]}"; do
  if ! listed "$name" "${processed[@]}" || listed "$name" "${checked[@]}"; then
    echo "::error::--readback-only 点名的 $name 不在要处理的桶 ${processed[*]} 里，或重复了" >&2
    exit 2
  fi
  checked+=("$name")
done

# 给了 --only 时只核对要上传的那几个桶用得到的凭据：国内机只传 cn，它的环境里没有、也不该有 R2 的任何变量
if [ "${#only[@]}" -eq 0 ]; then
  targets="$(node "$script_dir/dev-distribution.mjs" upload-targets "$R2_ACCOUNT_ID")"
else
  account=()
  if [ -n "${R2_ACCOUNT_ID:-}" ]; then
    account=("$R2_ACCOUNT_ID")
  fi
  targets="$(IFS=,; node "$script_dir/dev-distribution.mjs" upload-targets --only "${only[*]}" "${account[@]}")"
  while IFS=$'\t' read -r name profile _; do
    if listed "$name" "${readback_only[@]}"; then
      continue
    fi
    case "$profile" in
      oss) require OSS_CI_ACCESS_KEY_ID OSS_CI_ACCESS_KEY_SECRET ;;
      r2) require R2_CI_ACCESS_KEY_ID R2_CI_SECRET_ACCESS_KEY ;;
    esac
  done <<< "$targets"
fi

optional=""
if [ -n "$record" ]; then
  optional="$(node "$script_dir/dev-distribution.mjs" optional-mirrors "$key")"
  # 允许缺的桶失败时要写进作业摘要；缺这个变量说明不在 Actions 里，那就在传任何东西之前停下
  : "${GITHUB_STEP_SUMMARY:?给了上传记录就要有 GITHUB_STEP_SUMMARY，允许缺的桶失败时写在那里}"
  touch "$record"
fi

# 下面的配置项和参数是按 aws-cli v2 的文档写的；托管镜像哪天换成别的大版本，宁可在这里停下
aws_version="$(aws --version 2>&1)"
echo "$aws_version"
if ! [[ "$aws_version" =~ ^aws-cli/2\. ]]; then
  echo "::error::需要 aws-cli 2.x，实际是 $aws_version" >&2
  exit 1
fi

# 新版 CLI 默认给每次上传算 CRC 校验和，用 aws-chunked 分块编码带在尾部发出去，OSS 不支持分块编码，
# 所以只在操作要求时才算校验和，完整性改由 Content-MD5 保证。OSS 的 S3 兼容层要求桶名放在主机名里。
# 跨境上传容易遇到连接重置，标准重试模式最多试 5 次。
export AWS_CONFIG_FILE="${RUNNER_TEMP:?}/aws-config"
cat > "$AWS_CONFIG_FILE" <<'EOF'
[profile oss]
request_checksum_calculation = when_required
response_checksum_validation = when_required
retry_mode = standard
max_attempts = 5
s3 =
  addressing_style = virtual

[profile r2]
request_checksum_calculation = when_required
response_checksum_validation = when_required
retry_mode = standard
max_attempts = 5
EOF

md5="$(openssl md5 -binary "$file" | base64)"
file_name="$(basename "$file")"
channel="${key%%/*}"

# 每个桶的上传与读回共用 600 秒：单次请求的连接、读超时管不住时快时慢的跨境链路，托管机到杭州 OSS 慢起来一个包能传
# 二十多分钟，aws-cli 的重试又会让它一直传下去，直到作业时限把整个 publish 掐掉。平时一个包传进任一个桶只要几秒。
# 跨境连接偶尔会整个卡死：同一个包这次卡满时限，掐断重来几秒就传完。所以一次 put-object 最多 90 秒（平时几秒，
# 见过最慢的一次 46 秒），没传上就先读回（可能刚好写成），对不上再重传，直到只剩 120 秒留给最后一次读回。
# --kill-after 防它不理 TERM。到时限时 timeout 以 124 退出，再过 10 秒还没退、补发 KILL 时，timeout 连同自己所在的
# 进程组一起被杀，退出码是 137，两种都按超时处理。
bucket_budget=600
readback_floor=120
attempt_limit=90
# 剩下的上传时间不到这么多就不再重传：太短的一次多半也传不完，只会挤掉读回
attempt_floor=30
# 一个桶最多试这么多次：一报错就立刻失败的情形（凭据、权限）不会自己好，不该在时限里反复撞
max_attempts=4

summarize() {
  printf '**下载源降级**：%s\n' "$1" >> "$GITHUB_STEP_SUMMARY"
}

# 这个桶没传上，参数是原因：本通道允许缺的只降级、接着处理后面的桶，其余的退出 1
give_up() {
  if [ "$may_fail" = no ]; then
    echo "::error::$name：$1" >&2
    exit 1
  fi
  echo "::warning::$name：$1。$channel 通道允许缺 $name：$file_name 不算传上，后面的文件不再$verb $name，清单只列传上了更新包的源" >&2
  printf '%s\tfailed\t%s\n' "$name" "$key" >> "$record"
  summarize "$name 没有传上 \`$file_name\`（$1，用时 $((SECONDS - started)) 秒）。清单只列传上了更新包的源，其余源照发。"
}

while IFS=$'\t' read -r name profile bucket endpoint region; do
  # 只读回的桶不碰凭据：国内机传过 cn 之后，托管机这边不该再拿 cn 的凭据去写它
  if listed "$name" "${readback_only[@]}"; then
    verb=读回
  else
    verb=传
    case "$profile" in
      oss)
        key_id="$OSS_CI_ACCESS_KEY_ID"
        secret="$OSS_CI_ACCESS_KEY_SECRET"
        ;;
      r2)
        key_id="$R2_CI_ACCESS_KEY_ID"
        secret="$R2_CI_SECRET_ACCESS_KEY"
        ;;
      *)
        echo "::error::$name 的 profile $profile 没有对应的凭据" >&2
        exit 1
        ;;
    esac
  fi
  url="$(awk -F '\t' -v name="$name" '$1 == name { print $2 }' <<< "$public_urls")"
  test -n "$url"
  may_fail=no
  if [ -n "$record" ] && grep -qxF -e "$name" <<< "$optional"; then
    may_fail=yes
  fi

  # 跨境链路慢起来多半一阵子都慢：前一个文件在这个桶上没传上，这个文件不再白等一轮时限
  if [ "$may_fail" = yes ] && awk -F '\t' -v name="$name" '$1 == name && $2 == "failed" { found = 1 } END { exit !found }' "$record"; then
    echo "::warning::$name 在本次运行里已经失败过，$file_name 不再$verb $name" >&2
    printf '%s\tfailed\t%s\n' "$name" "$key" >> "$record"
    summarize "$name 在本次运行里已经失败过，没有再$verb \`$file_name\`。"
    continue
  fi

  started=$SECONDS
  deadline=$(( $(date +%s) + bucket_budget ))
  level=error
  if [ "$may_fail" = yes ]; then
    level=warning
  fi

  # 别处传过的对象只读回一次，整个 600 秒都给它：这里不重传，读回失败就是没传上
  if [ "$verb" = 读回 ]; then
    readback_status=0
    bash "$script_dir/check-download.sh" "$name" "$url" "$file" "$bucket_budget" "$level" < /dev/null || readback_status=$?
    if [ "$readback_status" -ne 0 ]; then
      give_up "由别处上传，按公网地址读回却和本地文件对不上，见上面读回的报错"
      continue
    fi
    if [ -n "$record" ]; then
      printf '%s\tok\t%s\n' "$name" "$key" >> "$record"
    fi
    echo "$name：由别处上传，按公网地址读回与本地一致，用时 $((SECONDS - started)) 秒"
    continue
  fi

  attempt=0
  while :; do
    attempt=$(( attempt + 1 ))
    limit=$(( deadline - $(date +%s) - readback_floor ))
    if [ "$limit" -gt "$attempt_limit" ]; then
      limit=$attempt_limit
    fi
    # 用 AWS_PROFILE 选配置段、凭据放环境变量：文档写明这种组合下环境变量里的凭据优先于 profile。
    # 对象路径带版本、永不改写，所以缓存一年并标 immutable。stdin 接 /dev/null，免得吃掉循环读的目标表。
    put_status=0
    AWS_PROFILE="$profile" AWS_ACCESS_KEY_ID="$key_id" AWS_SECRET_ACCESS_KEY="$secret" \
      timeout --kill-after=10 "$limit" \
      aws s3api put-object \
      --endpoint-url "$endpoint" \
      --region "$region" \
      --bucket "$bucket" \
      --key "$key" \
      --body "$file" \
      --content-type application/octet-stream \
      --cache-control 'public, max-age=31536000, immutable' \
      --content-md5 "$md5" \
      --cli-connect-timeout 30 \
      --cli-read-timeout 120 \
      --no-cli-pager < /dev/null || put_status=$?

    # put-object 报错也照样读回：服务端已经写成、响应却在跨境路上丢了时，aws-cli 按可重试错误重发同一个键，撞上桶的
    # 禁止覆盖写而报错，对象其实完好；被时限掐断时也可能刚好写完。CI 的凭据只能写、查不了对象，所以按客户端用的公网地址
    # 匿名读回，大小和 sha256 与本地一致才算传上；不按错误码分辨，OSS 的 S3 兼容层与 R2 对「已存在」各报什么码这里
    # 核实不了，比字节不依赖它。允许缺的桶读回失败只是警告，注解级别跟着降；这次读回之后还会重传时也只是警告，
    # 后面传上了的话作业成功，不该挂着错误注解。还会不会重传要读回之后才知道，读回耗掉的时间只会让它更不会，
    # 所以这里按读回之前的余量判，判成还会重传而实际没重传时，下面的失败行照样以错误报出。
    remaining=$(( deadline - $(date +%s) ))
    readback_level=$level
    if [ "$put_status" -ne 0 ] && [ "$attempt" -lt "$max_attempts" ] && [ $(( remaining - readback_floor )) -ge "$attempt_floor" ]; then
      readback_level=warning
    fi
    readback_status=0
    if [ "$remaining" -ge 1 ]; then
      bash "$script_dir/check-download.sh" "$name" "$url" "$file" "$remaining" "$readback_level" < /dev/null || readback_status=$?
    else
      echo "::$readback_level::$name：${bucket_budget} 秒的时限在读回之前就用完了：$url" >&2
      readback_status=1
    fi
    if [ "$readback_status" -eq 0 ]; then
      break
    fi
    # put-object 自己说成功、读回却对不上，是公共读或自定义域名的配置问题，重传也一样
    if [ "$put_status" -eq 0 ]; then
      break
    fi
    if [ "$attempt" -ge "$max_attempts" ] || [ $(( deadline - $(date +%s) - readback_floor )) -lt "$attempt_floor" ]; then
      break
    fi
    case "$put_status" in
      124 | 137) echo "$name：第 $attempt 次 put-object 超过 $limit 秒没有传完，读回也对不上，重传" ;;
      *) echo "$name：第 $attempt 次 put-object 报错，读回也对不上，重传" ;;
    esac
  done

  if [ "$readback_status" -eq 0 ]; then
    case "$put_status" in
      0) ;;
      124 | 137) echo "$name：第 $attempt 次 put-object 超过 $limit 秒没有传完，但公网读回与本地一致，按已传上处理" ;;
      *) echo "$name：第 $attempt 次 put-object 报错，但公网读回与本地一致，按已传上处理" ;;
    esac
    if [ "$attempt" -gt 1 ]; then
      echo "$name：试到第 $attempt 次才传上"
    fi
    if [ -n "$record" ]; then
      printf '%s\tok\t%s\n' "$name" "$key" >> "$record"
    fi
    echo "$name：已传到 $bucket/$key，用时 $((SECONDS - started)) 秒"
    continue
  fi

  case "$put_status" in
    0) failure="put-object 成功，按公网地址读回却和本地文件对不上，见上面读回的报错" ;;
    124 | 137) failure="put-object 试了 $attempt 次都没在时限内传完，按公网地址读回也和本地文件对不上" ;;
    *) failure="put-object 试了 $attempt 次都失败，按公网地址读回也和本地文件对不上，见上面 aws 的报错" ;;
  esac
  give_up "$failure"
done <<< "$targets"
