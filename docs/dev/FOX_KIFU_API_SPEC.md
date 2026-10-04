# 野狐围棋（foxwq）棋谱查询 API Spec

匿名查询公开对局；无官方文档或 SLA，字段和接口可能变化。`hide_game_record=1` 的账号不会返回对局。
请求参数按 UTF-8 URL 编码；响应按 UTF-8 JSON 解码（响应头未必声明 charset）。
以下样本于 2026-08-12 测得，示例账号为柯洁（uid `6757425`）。

## 1. 接口总览

| # | 用途 | 方法 | URL |
| --- | --- | --- | --- |
| 1 | 昵称 → uid / 账号资料 | GET | `https://newframe.foxwq.com/cgi/QueryUserInfoPanel` |
| 2 | 对局列表 | GET | `https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList` |
| 3 | 单局 SGF | GET | `https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess` |

已有 uid 则直接取列表，否则先用昵称查 uid；从列表的 `chessid` 逐局取 SGF。

共用 HTTP 约定见 [§7](#7-错误处理与调用约定)，弈城与弈客也沿用。

## 2. 账号查询 `QueryUserInfoPanel`

```
GET https://newframe.foxwq.com/cgi/QueryUserInfoPanel?srcuid=0&username={nickname}
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `username` | 是 | 野狐昵称，**精确匹配**，不是模糊搜索 |
| `srcuid` | 是 | 请求方 uid，匿名固定 `0` |

响应节选：

```json
{
  "result": 0,
  "uid": "6757425",
  "username": "柯洁",
  "dan": 108,
  "occupation": 2,
  "hide_game_record": 0
}
```

`uid` 是字符串；`hide_game_record=1` 表示该账号不公开棋谱。`dan` 和 `occupation`
的换算见 §5.1。昵称不存在时返回非零 `result`（或 `errcode`），错误文案在
`resultstr` 或 `errmsg`。

## 3. 对局列表 `YHWQFetchChessList`

```
GET https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList
    ?dstuid=6757425&type=1&fetchnum=200
```

### 3.1 参数

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `dstuid` | 是 | 账号 uid；`0` 为全站最新对局流 |
| `type` | 是 | `1` 最近对局；`2` 职业 / 官方对局（`professional=1`）；`3` 当日对局 |
| `fetchnum` | 否 | 默认约 101；建议显式传 `200`，服务端硬上限 200 |

`type=2` 在示例职业账号返回 200 条，与 `type=1` 重合 199 条；业余账号返回空。
`type=3` 在当天无对局时返回空。其他试过的类型无用：`4` 与 `1` 相同，
`0` 返回错误，`5`～`7` 返回空。`srcuid` 可省略（默认 `0`）；
`uin`、`searchkey` 无可观察效果，`lastcode` 无法分页（§3.3）。

### 3.2 响应

```json
{
  "result": 0,
  "chesslist": []
}
```

`result=0` 表示成功；`chesslist` 是 §5 的记录数组，空数组表示无公开对局。
响应的 `lastcode` 恒为 `0`，不能当游标。

### 3.3 分页与可取范围

`dstuid=6757425`、`fetchnum=200` 的连续请求：`lastcode=0` 返回 200 条，
首尾 `chessid` 分别为 `1785337045010001403`、`1668325157010002153`；
把末条 `chessid` 当下一次的 `lastcode`，仍得到同一批 200 条、响应游标仍为 `0`。
改用时间戳或数字下标也无效。在约 26,000 局的账号（uid `211958`）上复核，
`fetchnum>200` 仍只返回 200 条。

**每种 `type` 只能取最近最多 200 条，无法翻更早历史。**要保存更长历史，定期抓取，
本地按 `chessid` 去重。

## 4. 单局棋谱 `YHWQFetchChess`

```
GET https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess?chessid={chessid}
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `chessid` | 是 | 列表记录中的 `chessid` |

响应结构示意（略去其他字段）：

```json
{
  "result": 0,
  "chessid": "1785337045010001403",
  "chess": "(;GM[1]FF[4]\\r\\nSZ[19]KM[375];B[pd])"
}
```

`chess` 是 SGF 全文；解码 JSON 后，其中的 `\r\n` 仍是反斜杠加字母，
须按 §6.2 处理。公开对局的 `chessid` 均可取，不限于目标账号。

## 5. 对局记录字段字典

`chesslist` 记录节选（柯洁对党毅飞，略去其他字段）：

```json
{
  "chessid": "1785337045010001403",
  "blackuid": 6757425, "blacknick": "柯洁", "blackdan": 108, "blackocc": 2,
  "whiteuid": 7093195, "whitenick": "党毅飞", "whitedan": 108, "whiteocc": 2,
  "professional": 1, "winner": 1, "point": -1, "rule": 1,
  "movenum": 205, "boardsize": 19, "handicap": 0, "komi": 375,
  "starttime": "2026-07-29 22:57:25",
  "title": "第6届中国围棋王中王争霸赛总决赛<张学斌＆夏夏＆绝艺解说>",
  "sgf": ""
}
```

| 字段 | 语义 |
| --- | --- |
| `chessid` | 字符串主键；取 SGF、去重都用它，不要按 int64 解析再拼接 |
| `blackuid` / `whiteuid` | 数字 uid；账号接口的 `uid` 则是字符串 |
| `blacknick` / `whitenick` | 昵称可能为空，可回退到 `blackenname` / `whiteenname` |
| `blackdan` / `whitedan`、`blackocc` / `whiteocc` | 段位及身份码，见 §5.1 |
| `professional` | `1` 表示职业 / 官方对局 |
| `winner` / `point` | `winner=1` 黑胜、`2` 白胜，其它值无胜负；`point=-1` 认输、`-2` 超时，其他负值表示非计点胜；非负数为百分之一单位的净胜（`point/100`） |
| `rule` / `komi` | `rule=1` 中国规则（子）、`0` 日韩规则（目）；`komi` 是百分之一子（`375` = 3.75 子）；`rule` 决定 `point/100` 的单位 |
| `handicap` | `0` 分先、`1` 让先（无座子）、`≥2` 让子；座子以 SGF 为准（§6.3） |
| `starttime` / `endtime` | 东八区 `yyyy-MM-dd HH:mm:ss`；`gamestarttime` / `gameendtime` 为对应的 Unix 秒字符串 |
| `sgf` | 列表里为空串；取棋谱需调 §4 |

`movenum` 为总手数，`boardsize` 为棋盘大小，`title` 是赛事标题（普通对局为空）。
未列出的字段以实际响应为准，不应依赖其未验证的语义。

### 5.1 段位编码换算

业余账号 `level = dan - 17`：`level > 0` 为段数（`dan=23` 即 6 段），
否则为 `1 - level` 级（`dan=3` 即 15 级）；与 SGF 的 `BR[]` / `WR[]` 文本一致。
职业身份由 `occupation` 或 `blackocc` / `whiteocc` 的非零值确定，
不用 `dan` 阈值；职业段数 1～9 用 `dan-99` 表示（如 `dan=108` 为职业九段，
SGF 中写作 `P9段`），其它取值不可据此换算。

## 6. SGF 格式注意事项

SGF 使用野狐方言；供通用 SGF 解析器读取前，需处理以下规则。

### 6.1 贴目 `KM` 归一化

根节点 `KM[375]` 表示 3.75 子，折合 7.5 目。归一化时仅处理第一棵树的根节点：
若数值绝对值 ≥200，先除以 100；再仅将绝对值 ≤4、尾数为 `.25` 或 `.75` 的数值
乘 2（子换成目），保留负号。`KM[750]` 和已用目表示的 `KM[7.5]`
都应保持为 7.5 目；让子 / 让先局通常为 `KM[0]`。

### 6.2 字面 `\r\n` 转义与多余反斜杠

解码 JSON 后，SGF 的 `\r`、`\n`、`\t` 仍是反斜杠加字母，
属性之间和属性值内部都可能出现。例如：

```
(;GM[1]FF[4]\r\nSZ[19]C[首行\n次行];B[pd])
```

先去开头 BOM，再把上述三个转义分别转为真正的 CR、LF、制表符，
**属性值内外都要转换**：若只丢反斜杠，分支之间残留 `rn`，注释换行变成字母 `n`。
值外的其它反斜杠也要丢弃；值内的其它转义对（如 `\]`、`\\`、
`\中`）原样保留，由 SGF 解析器处理。值内已有的真实换行保留。

### 6.3 让子棋座子写成落子

野狐可将座子写成开局连续的单值节点 `;AB[dd];AB[pd];AB[dp]`，
也可能写成连续 ≥2 手黑棋；根节点 `HA[]` 不总可靠。三子局结构示意：

```
(;GM[1]SZ[19]KM[0]HA[3];AB[dd];AB[pd];AB[dp];W[dm];...)
```

把开局连续的座子合并进根节点 `AB[dd][pd][dp]`，据实际座子数设置
`HA[n]`，并把 `KM` 置 `0`；后续分支保留。否则解析器可能把座子
当作连续黑棋落子，或只保留第一颗。

### 6.4 分支、讲解与其它方言

- 带讲解的对局（`commenttype=1`）SGF 是**多分支树**：`;B[pd]C[...]\r\n(;W[dd]\r\n(;B[pq]...`，主线之外挂着大量变化图与解说 `C[]`。只做复盘的接入方应先抽取主线。
- 根节点可能出现两个 `AP[]`（`AP[GNU Go:3.8]` 与 `AP[foxwq]`）。
- 非标准属性：`RN`、`RL`、`TC`、`TT`（读秒次数 / 每次读秒秒数），`TM` 为基本用时秒数。
- `BR[]` / `WR[]` 是中文段级文本（`6段`、`15级`、职业 `P9段`）。
- `RE[]` 形如 `W+R`、`B+R`、`W+3.5`、`W+0`，与列表里的 `winner` / `point` 一致，可交叉校验。

## 7. 错误处理与调用约定

- JSON 的 `result=0` 表示成功；账号接口还可能返回 `errcode`，非零亦为错误。错误文案在 `resultstr` 或 `errmsg`。
- HTTP 200 不代表业务成功；非 JSON、截断或缺失 `chess` 的响应均按失败处理。
- UA 用 [`kifu::user_agent()`](../../crates/mirai-client/src/kifu.rs) 的返回值，不另存副本。[GTK 的 `Soup` 传输](../../crates/mirai/src/kifu.rs) 只显式设置 UA，不额外设置 `Accept`；超时和重试以该传输为准。运行这三份 spec 的 curl 示例前，将 shell 变量 `UA` 设为该函数返回值。
- 批量抓取建议串行，每局间隔 ≥ 0.5 秒，避免公共接口限流；这不是 GTK 传输自动施加的间隔。

## 8. 最小调用示例

```bash
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

## 9. 合规提醒

- 数据归野狐围棋及棋手所有；只取公开对局，不绕过 `hide_game_record`，不要批量刷全站流。
