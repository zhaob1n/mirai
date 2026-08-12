# 野狐围棋（foxwq）棋谱查询 API Spec

面向第三方接入的接口说明。本文只描述 HTTP 接口本身与数据格式，不涉及任何具体实现代码。

- 服务提供方：野狐围棋（腾讯围棋 / foxwq），非官方公开文档，无稳定性承诺。
- 认证：**不需要**。列表与棋谱接口匿名可用。
- 数据范围：仅公开对局。用户在客户端隐藏棋谱后（账号信息里 `hide_game_record=1`）列表为空。
- 编码：请求参数 UTF-8 URL 编码，响应 UTF-8 JSON（响应头不一定带 charset，按 UTF-8 解码）。
- 实测环境：2026-08-12。本文示例统一使用公开职业账号 `柯洁`（uid `6757425`）；涉及取数上限的验证另用一个约 26,000 局的公开账号（uid `211958`）复核。

## 1. 接口总览

| # | 用途 | 方法 | URL |
| --- | --- | --- | --- |
| 1 | 昵称 → uid / 账号资料 | GET | `https://newframe.foxwq.com/cgi/QueryUserInfoPanel` |
| 2 | 对局列表 | GET | `https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList` |
| 3 | 单局 SGF | GET | `https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess` |

典型流程：昵称 →（1）取 uid →（2）取对局列表 →（3）按 `chessid` 逐局取 SGF。
调用方若已有 uid，可跳过第 1 步。

建议请求头（移动端 UA 兼容性最好）：

```
User-Agent: Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1
Accept: application/json,text/plain,*/*
```

## 2. 账号查询 `QueryUserInfoPanel`

```
GET https://newframe.foxwq.com/cgi/QueryUserInfoPanel?srcuid=0&username={nickname}
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `username` | 是 | 野狐昵称，**精确匹配**，不是模糊搜索 |
| `srcuid` | 是 | 请求方 uid，匿名固定 `0` |

响应（节选，实际字段 80+）：

```json
{
  "result": 0,
  "uid": "6757425",
  "username": "柯洁",
  "englishname": "KeJie",
  "dan": 108,
  "occupation": 2,
  "country": 86,
  "gender": 0,
  "registertime": "1493682655",
  "totalwin": 3,
  "totallost": 0,
  "totalequal": 0,
  "ai": 0,
  "hide_game_record": 0,
  "avatar_url": "..."
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `result` | int | `0` 成功；部分错误响应改用 `errcode` |
| `resultstr` / `errmsg` | string | 失败原因 |
| `uid` | string | 账号数字 ID（字符串形态），列表接口用它 |
| `username` / `englishname` | string | 昵称 / 英文名 |
| `dan` | int | 段位编码，换算见 §5.1；职业账号是另一套编码（示例中 `108`） |
| `occupation` | int | `0` 业余；非 0 为职业 / 认证身份（示例职业九段为 `2`） |
| `registertime` | string | Unix 秒 |
| `totalwin` / `totallost` / `totalequal` | int | 该账号累计胜 / 负 / 和；与列表可取条数无关（职业账号线上对局很少，示例为 3:0） |
| `hide_game_record` | int | `1` 表示用户隐藏棋谱，列表将为空 |

错误判定：`result != 0`（或存在 `errcode != 0`）。昵称不存在时返回非 0 与错误文案。

## 3. 对局列表 `YHWQFetchChessList`

```
GET https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList
    ?dstuid=6757425&type=1&fetchnum=200
```

### 3.1 参数

| 参数 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- |
| `dstuid` | 是 | — | 目标账号 uid。**传 `0` 返回全站最新对局流**（不限用户） |
| `type` | 是 | — | 列表类型，见 §3.2。缺省会返回 `result=1` |
| `fetchnum` | 否 | ≈101 | 单次返回条数上限，**服务端硬上限 200**，超过按 200 处理；`0` 视为默认 |
| `srcuid` | 否 | `0` | 请求方 uid，可省略 |
| `uin` | 否 | — | 历史参数，无可观察影响 |
| `lastcode` | 否 | `0` | 名义翻页游标，**实测完全无效**，见 §3.4 |
| `searchkey` | 否 | 空 | 名义关键字过滤，**实测完全无效**（传对手昵称，条数与内容都不变） |

### 3.2 `type` 取值（实测）

| type | 含义 | 实测样本（`dstuid=6757425`） |
| --- | --- | --- |
| `1` | 该账号最近对局（常用） | 200 条（触上限），时间跨度 2022-11 ~ 2026-07 |
| `2` | 职业 / 官方对局（记录中 `professional=1`） | 200 条，与 `type=1` 重合 199 条；业余账号返回 0 条 |
| `3` | 当日对局 | 该账号当天无对局时为 0 条 |
| `4` | 与 `1` 结果一致 | — |
| `0`,`5`,`6`,`7` | 无效 / 空 | `0` 返回 `result=1`，其余返回空数组 |

### 3.3 响应

```json
{
  "result": 0, "resultstr": "", "ret": 0,
  "srcuid": "0", "dstuid": "6757425", "uin": "0",
  "type": 1, "lastcode": 0, "searchkey": "", "trans": "",
  "chesslist": [ /* 见 §5 */ ]
}
```

成功判定：`result == 0`。`chesslist` 为空数组表示无公开对局（或该 `type` 下无数据）。
响应中的 `lastcode` 恒为 `0`，不要当游标用。

### 3.4 分页与可取范围（重要）

`lastcode` 不是可用游标。实测（`dstuid=6757425`，`fetchnum=200`，连续 3 次请求，依次传 `0`、上一页最后一条 `chessid`、再上一页最后一条 `chessid`）：

```
p1 lastcode=0                    n=200 first=1785337045010001403 last=1668325157010002153 echo=0
p2 lastcode=1668325157010002153  n=200 first=1785337045010001403 last=1668325157010002153 echo=0
p3 lastcode=1668325157010002153  n=200 first=1785337045010001403 last=1668325157010002153 echo=0
```

窗口始终从最新一局开始且不前移；换成 `gamestarttime`、`chessmodifytime`、数字下标同样无效。在一个约 26,000 局的账号（uid `211958`）上复核结论一致，且 `fetchnum` 超过 200 仍只返回 200 条。

结论：**只能取到每个 `type` 下最近 `fetchnum`（≤200）条对局，无法翻更早历史。**
需要更长历史只能定期增量抓取，在本地按 `chessid` 去重累积。不传 `fetchnum` 时默认约 101 条，接入方应显式传 `fetchnum=200`。

## 4. 单局棋谱 `YHWQFetchChess`

```
GET https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess?chessid={chessid}
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `chessid` | 是 | 列表记录中的 `chessid` |

响应：

```json
{
  "result": 0,
  "chessid": "1785337045010001403",
  "flag": 1,
  "chess": "(;GM[1]FF[4]\\r\\nSZ[19]\\r\\nGN[第6届中国围棋王中王争霸赛总决赛]\\r\\nDT[2026-07-30]\\r\\nPB[柯洁]\\r\\nPW[党毅飞]\\r\\nBR[P9段]\\r\\nWR[P9段]\\r\\nKM[375]HA[0]RU[Chinese]AP[GNU Go:3.8]RE[B+R]TM[7200]TC[5]TT[60]AP[foxwq]RL[0]\\r\\n;B[pd]C[...]\\r\\n(;W[dd]...",
  "srcuid": "0",
  "trans": ""
}
```

| 字段 | 说明 |
| --- | --- |
| `result` | `0` 成功 |
| `chess` | SGF 全文，注意里面的 `\r\n` 是**字面两字符转义**，不是换行，见 §6.2 |
| `flag` | 实测恒为 `1`，含义未知 |

任意公开对局都可取，不限于某个账号；全站流（`dstuid=0`）里其他棋手的 `chessid` 同样可取。

## 5. 对局记录字段字典

`chesslist` 单条记录实测全字段（示例：柯洁 vs 党毅飞，第 6 届中国围棋王中王争霸赛总决赛）：

```json
{
  "chessid": "1785337045010001403",
  "blackuid": 6757425, "blacknick": "柯洁", "blackenname": "KeJie",
  "blackdan": 108, "blackcountry": 86,
  "whiteuid": 7093195, "whitenick": "党毅飞", "whiteenname": "党毅飞",
  "whitedan": 108, "whitecountry": 86,
  "professional": 1,
  "title": "第6届中国围棋王中王争霸赛总决赛<张学斌＆夏夏＆绝艺解说>",
  "gamestarttime": "1785337045", "gameendtime": "1785402525",
  "chessmodifytime": "1785402525", "recordmodifytime": "0",
  "starttime": "2026-07-29 22:57:25", "endtime": "2026-07-30 17:08:45",
  "winner": 1, "point": -1, "reason": 3, "rule": 1,
  "movenum": 205, "boardsize": 19, "handicap": 0, "firstcolor": 0, "komi": 375,
  "gametype": 5, "additionalrule": 0, "favorite": 0, "introduction": "",
  "blackocc": 2, "whiteocc": 2, "commenttype": 1, "matchid": 0, "clienttype": 1,
  "recorderuid": 0, "recordernick": "", "maxonline": 12441,
  "commentcount": 0, "viewcount": 0,
  "black_ye6": 0, "black_ai": 0, "black_gender": 0,
  "white_ye6": 0, "white_ai": 0, "white_gender": 0,
  "black_fc_occ": 0, "black_fc_grade": 0, "white_fc_occ": 0, "white_fc_grade": 0,
  "sgf": "", "jueyi_replay": 1
}
```

| 字段 | 类型 | 语义 |
| --- | --- | --- |
| `chessid` | string | 棋谱主键；取 SGF、去重都用它。**是字符串，不要按 int64 解析后再拼接** |
| `blackuid` / `whiteuid` | number | 双方 uid（这里是数字，账号接口里的 `uid` 是字符串） |
| `blacknick` / `whitenick` | string | 昵称；部分记录为空，可回退 `blackenname` / `whiteenname` |
| `blackdan` / `whitedan` | int | 段位编码，见 §5.1 |
| `blackocc` / `whiteocc` | int | 身份码，与账号接口 `occupation` 同源；`0` 业余，非 0 职业 |
| `professional` | int | `1` 职业 / 官方对局 |
| `winner` | int | `1` 黑胜，`2` 白胜；其它值表示无胜负（和棋 / 未结算 / 异常） |
| `point` | int | `-1` 中盘胜，`-2` 超时胜，其它负值为其它非计点胜；`>= 0` 时为**百分之一单位**净胜（`point/100`） |
| `rule` | int | `1` 中国规则（单位「子」），`0` 日韩规则（单位「目」）；决定 `point/100` 的单位 |
| `reason` | int | 结束原因码，实测认输局为 `3`；其余取值未穷举，建议以 `point` 为准 |
| `komi` | int | **百分之一单位**：`375` = 3.75 子（中国规则）；让子 / 让先局为 `0` |
| `movenum` | int | 总手数 |
| `boardsize` | int | 棋盘大小（19 / 13 / 9） |
| `handicap` | int | `0` 分先（有贴目，`komi=375`）；`1` 让先（无座子，`komi=0`）；`>=2` 让子局，与 SGF `HA[n]` 一致。座子坐标以 SGF 为准，见 §6.3 |
| `firstcolor` | int | 先行方标记，实测职业对局为 `0`、普通对局为 `1`，语义未验证，不建议依赖 |
| `starttime` / `endtime` | string | 服务端本地时间 `yyyy-MM-dd HH:mm:ss`（东八区） |
| `gamestarttime` / `gameendtime` | string | 同一时刻的 Unix 秒（字符串） |
| `chessmodifytime` / `recordmodifytime` | string | 棋谱最后修改时间，Unix 秒；`0` 表示无 |
| `gametype` | int | 对局类型，实测普通对局 `1` / `2`，职业赛事 `4` / `5` / `6`；完整取值表未公开 |
| `title` / `introduction` | string | 赛事标题 / 简介，普通对局为空 |
| `commenttype` | int | `1` 表示带讲解（SGF 里会有大量 `C[]` 与分支） |
| `maxonline` | int | 观战人数峰值 |
| `commentcount` / `viewcount` | int | 评论数 / 观看数 |
| `black_ai` / `white_ai` | int | AI 标记 |
| `black_gender` / `white_gender` | int | 性别标记 |
| `jueyi_replay` | int | 是否有绝艺复盘 |
| `sgf` | string | **列表里恒为空串**，SGF 必须单独调 §4 |

未列出的字段（`*_fc_grade`、`additionalrule`、`clienttype`、`matchid` 等）语义未公开，接入方不要依赖。

### 5.1 段位编码换算

业余账号：

```
level = dan - 17
level > 0  → level 段（例：dan=23 → 6 段）
level <= 0 → |level| + 1 级（例：dan=3 → 15 级）
```

与 SGF 的 `BR[]` / `WR[]` 文本一致（实测 `dan=23` ↔ `WR[6段]`，`dan=3` ↔ `BR[15级]`）。

职业账号不适用该公式：`occupation`（或记录里的 `blackocc` / `whiteocc`）非 0 时 `dan` 是另一套编码（示例职业九段 `dan=108`），SGF 中写作 `BR[P9段]` / `WR[P9段]`。判断职业身份用 `professional` / `*occ`，不要用 `dan` 阈值。

## 6. SGF 格式注意事项

野狐返回的 SGF 是非标准方言，直接喂给通用解析器会出问题。接入方需要处理以下四点。

### 6.1 贴目 `KM` 是百分之一单位

`KM[375]` 表示 3.75（中国规则「子」）。换算成常见的「目」需要乘 2 → 7.5 目。
建议规则：`KM >= 200` 时先除以 100；若结果落在 `[-4, 4]` 再乘 2；小数以 `.25` / `.75` 结尾的同样乘 2。让子 / 让先局通常为 `KM[0]`。

### 6.2 字面 `\r\n` 转义与多余反斜杠

带讲解的棋谱会在属性之间插入**字面两字符** `\` + `r`、`\` + `n`（不是真正的 CR/LF）。示例职业对局的 SGF 里出现 762 处：

```
(;GM[1]FF[4]\r\nSZ[19]\r\nGN[...]\r\nDT[2026-07-30]\r\nPB[柯洁]\r\nPW[党毅飞]\r\nBR[P9段]\r\nWR[P9段]\r\nKM[375]HA[0]...
```

标准 SGF 解析器会把属性值外的 `\` 当转义符，导致节点错乱或解析失败。
处理：先去 BOM；逐字符扫描，**属性值 `[...]` 外部的 `\` 一律丢弃**，属性值内部保留转义对（`\]`、`\\`）。注意 `C[]` 注释值内部本身可能包含真实换行，不要一起清掉。

### 6.3 让子棋座子写成落子

野狐把让子座子写成**开局连续多个单值节点** `;AB[dd];AB[pd];AB[dp]`，也可能是开局连续多手 `B[]`，并且根节点 `HA[]` 不总可靠。

实测三子局（全站流中的匿名对局，双方昵称为默认占位名）：

```
(;GM[1]FF[4]SZ[19]GN[]DT[2026-08-12]PB[黑方棋手]PW[白方棋手]BR[18级]WR[18级]KM[0]HA[3]RU[Chinese]AP[GNU Go:3.8]RN[0]RE[W+0]TM[0]TC[0]TT[0]AP[foxwq]RL[0];AB[dd];AB[pd];AB[dp];W[dm];B[cn];...)
```

处理：把开局连续的 `AB` 单值节点（或连续 ≥2 手黑棋）提升为根节点 `HA[n]` + 单节点多值 `AB[dd][pd][dp]`，并把 `KM` 置 `0`。否则严格交替落子的解析器会把座子当成正常落子，或只保留第一颗、丢掉其余座子。

### 6.4 分支、讲解与其它方言

- 带讲解的对局（`commenttype=1`）SGF 是**多分支树**：`;B[pd]C[...]\r\n(;W[dd]\r\n(;B[pq]...`，主线之外挂着大量变化图与解说 `C[]`。只做复盘的接入方应先抽取主线。
- 根节点可能出现两个 `AP[]`（`AP[GNU Go:3.8]` 与 `AP[foxwq]`）。
- 非标准属性：`RN`、`RL`、`TC`、`TT`（读秒次数 / 每次读秒秒数），`TM` 为基本用时秒数。
- `BR[]` / `WR[]` 是中文段级文本（`6段`、`15级`、职业 `P9段`）。
- `RE[]` 形如 `W+R`、`B+R`、`W+3.5`、`W+0`，与列表里的 `winner` / `point` 一致，可交叉校验。

## 7. 错误处理与调用约定

- 统一成功判定：`result == 0`（列表接口另有 `ret` 字段，实测恒为 `0`）。
- 失败时文案在 `resultstr`（账号接口可能是 `errmsg`）。
- HTTP 层可能返回 200 但 `result != 0`；也可能返回非 JSON（网关异常），解析失败要按失败处理，不要把截断内容当成功。
- 建议：连接超时 20s，读超时 25s，失败重试 3 次，退避 350ms × 尝试次数。
- 建议串行请求并限速（每局 SGF 之间 ≥0.5s）；这些是公共接口，批量高频抓取容易被限流。
- 响应体请一次性读完再解码，避免中途 IO 异常导致 JSON 截断被误判为成功。

## 8. 已废弃 / 不可用的相关端点

以下端点在历史客户端里用于取 SGF，**当前匿名调用不可用**，新接入不要实现：

```
POST http://happyapp.huanle.qq.com/cgi-bin/CommonMobileCGI/TXWQFetchChess
POST http://cgi.foxwq.com/cgi-bin/CommonMobileCGI/TXWQFetchChess
Content-Type: application/x-www-form-urlencoded
chessid={chessid}
```

实测：`happyapp` 返回 `{"result":-3,"resultstr":"FetchChessFromDB Failed!!"}`，`cgi.foxwq.com` 返回非 JSON。

## 9. 腾讯围棋（`huanle.qq.com`）链路：需要登录态

同源的腾讯围棋接口，形态与野狐一致，但**必须携带有效会话**：

```
GET https://cgi.huanle.qq.com/cgi-bin/CommonMobileCGI/TXWQFetchChessList
    ?type=7&lastCode=0&username={name}&srcuid={uid或空}&txwqsession={session}&fetchnum=100

GET https://happyapp.huanle.qq.com/cgi-bin/CommonMobileCGI/TXWQFetchChess?chessid={chessid}
```

- 注意参数名是驼峰 `lastCode`（野狐是全小写 `lastcode`）。
- 成功判定 `result == 0 && ret == 0`；数据结构与野狐同构（`chesslist` / `chess`）。
- 实测无有效 `txwqsession` 时：列表返回 `{"result":0,...,"chesslist":[]}`，详情返回 `{"result":-3,...}`。
- 会话只能来自腾讯围棋官方客户端登录，本 spec 不覆盖其获取方式。

## 10. 最小调用示例

```bash
UA='Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1'

# 1) 昵称 -> uid
curl -s -A "$UA" --get --data-urlencode 'username=柯洁' \
  'https://newframe.foxwq.com/cgi/QueryUserInfoPanel?srcuid=0'

# 2) 最近对局（最多 200 条）
curl -s -A "$UA" \
  'https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList?dstuid=6757425&type=1&fetchnum=200'

# 3) 单局 SGF
curl -s -A "$UA" \
  'https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess?chessid=1785337045010001403'

# 附：全站最新对局流
curl -s -A "$UA" \
  'https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList?dstuid=0&type=1&fetchnum=200'
```

Python 参考流程（标准库即可）：

```python
import json, time, urllib.parse, urllib.request

UA = ("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 "
      "(KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1")
FOX = "https://h5.foxwq.com/yehuDiamond/chessbook_local"


def get_json(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=25) as resp:
        return json.loads(resp.read().decode("utf-8", "replace"))


def resolve_uid(nickname):
    if nickname.isdigit():
        return nickname
    q = urllib.parse.quote(nickname)
    data = get_json(f"https://newframe.foxwq.com/cgi/QueryUserInfoPanel?srcuid=0&username={q}")
    if data.get("result", data.get("errcode", -1)) != 0:
        raise RuntimeError(data.get("resultstr") or data.get("errmsg") or "user not found")
    return str(data["uid"])


def list_games(uid, type_=1, fetchnum=200):
    data = get_json(f"{FOX}/YHWQFetchChessList?dstuid={uid}&type={type_}&fetchnum={fetchnum}")
    if data.get("result") != 0:
        raise RuntimeError(data.get("resultstr") or "list failed")
    return data.get("chesslist", [])


def fetch_sgf(chessid):
    data = get_json(f"{FOX}/YHWQFetchChess?chessid={urllib.parse.quote(str(chessid))}")
    return data.get("chess", "") if data.get("result") == 0 else ""


uid = resolve_uid("柯洁")
for game in list_games(uid):
    sgf = fetch_sgf(game["chessid"])     # 之后按 §6 做 SGF 归一化
    time.sleep(0.5)
```

## 11. 合规提醒

- 数据归野狐围棋及棋手所有，仅取公开对局；不要绕过 `hide_game_record`。
- 无官方文档与 SLA，字段和可用性可能随时变化；对 `result`、缺字段、空 `chess` 都要做防御。
- 请自觉限速，不要批量刷全站流。
