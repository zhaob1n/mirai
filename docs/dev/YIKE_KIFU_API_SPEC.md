# 弈客围棋棋谱查询 API Spec

匿名查询公开对局；无官方文档或 SLA。职业棋手和注册账号是两套 id，不能混用。

请求参数按 UTF-8 URL 编码。棋手库响应是 `application/json`（不声明 charset），中文写成 `\uXXXX`，按 JSON 解码。`api.yikeweiqi.com` 的响应头是 `application/json; charset=utf-8`，正文是原始 UTF-8。

mirai 只带手机 UA，不带签名头。以下端点于 2026-10-04 用该 UA、无其它头复测通过。棋手库示例为柯洁（pid `1195`）。账号示例为沈尧（id `1`，弈客号 `CGF00001`）。

建议请求头：

```
User-Agent: Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1
Accept: application/json,text/plain,*/*
```

## 1. 接口总览

| # | 用途 | 方法 | URL |
| --- | --- | --- | --- |
| 1 | 棋手名 → pid | GET | `https://home.yikeweiqi.com/player/api/player_name_search` |
| 2 | pid → 对局列表 | GET | `https://home.yikeweiqi.com/player/api/game_search_player` |
| 3 | gid → 棋谱 | GET | `https://home.yikeweiqi.com/player/api/game_sgf` |
| 4 | 昵称 / 弈客号 → 账号 id | GET | `https://api.yikeweiqi.com/reguser/search` |
| 5 | 账号 id → 在线对局 | GET | `https://api.yikeweiqi.com/reguser/games/{id}/1/{page}` |
| 6 | GameId → SGF | GET | `https://api.yikeweiqi.com/usersgf/detail` |

职业棋手走 1 → 2 → 3。注册账号走 4 → 5 → 6。页面上的「弈客号」是 `cgf_id`，不是 §6 路径里的数字 id。

桌面端自己的 `friend/search` 必须登录（`Status=1404`）。当前 SPA（2026-09-30）没有引用接口 4（`reguser/search`）；它是匿名可用的替代。

## 2. 棋手名搜索 `player_name_search`

```
GET https://home.yikeweiqi.com/player/api/player_name_search?key={name}
```

`key` 是前缀，不是精确匹配：`柯` 会带出柯洁以外的人。调用方只留 `name` 与查询全等的行。无命中是 `{"matches":[]}`，HTTP 200。

```json
{"matches":[{"name":"\u67ef\u6d01","pid":"1195"}]}
```

`pid` 是字符串。

## 3. 棋手对局 `game_search_player`

```
GET https://home.yikeweiqi.com/player/api/game_search_player?pid=1195&start=0
```

`start` 是偏移，不是页码。每页固定 20 条。柯洁 `total=938`：`start=0` 20 条，`start=920` 18 条，`start=938` 空列表且 `total` 不变。hex 棋手页 pid 不能直接用，会得到 `total: null`。

```json
{
  "total": 938,
  "games": [{
    "id": "128199",
    "event": "第6届嵊州杯中国王中王争霸赛决赛",
    "date": "2026-07-30",
    "black": "柯洁", "black_rank": "九段",
    "white": "党毅飞", "white_rank": "九段",
    "komi": "7.5", "result": "黑中盘胜"
  }]
}
```

列表没有手数和棋盘大小。`black_rank` 是中文段位（`九段`、`初段`）。`komi` 已经是目，不要除 100。`date` 只有 `YYYY-MM-DD`。

## 4. 棋手谱 `game_sgf`

```
GET https://home.yikeweiqi.com/player/api/game_sgf?gid=128199
```

成功没有 `code`。不存在是 HTTP 200 且 `code==1`，文案在 `msg`。

```json
{
  "komi": "7.5",
  "black": "柯洁", "black_rank": "9p", "black_id": "1195",
  "white": "党毅飞", "white_rank": "9p",
  "result": "黑中盘胜",
  "date": "2026-07-30",
  "sgf": "(;EV[];B[pd];W[dd];...)"
}
```

`sgf` 是标准 SGF 坐标（19 路 `a`–`s`，含 `i`，天元 `jj`），但根节点只有空的 `EV[]`。贴目、姓名、段位、结果、日期都在 JSON 里。详情段位是 `9p`，与列表的 `九段` 不是同一种写法。柯洁对党毅飞 205 手。

## 5. 账号搜索 `reguser/search`

```
GET https://api.yikeweiqi.com/reguser/search?condition={text}&page=1
```

不需要签名。`condition` 同时匹配昵称、拼音和弈客号，很宽：`沈` 有 36090 条，`柯洁` 有 425 条，其中多条 `nickname` 全等。`CGF00001` 只命中 id `1`。无命中是 `Status=1200`、`total=0`。

调用方只留昵称与查询忽略大小写全等、或 `cgf_id` 忽略大小写全等的行。重名时接口不标明哪条是官方号；柯洁 `10.5D` / `CGF01324` 的 id 是 `1323`，其在线列表有对局。

分页参数是 `page`（从 1），每页 20。`p` 被忽略。mirai 只取第 1 页。

响应里有邮箱、生日等。只取 `id`、`cgf_id`、`nickname`、`grade`，不要保存其余字段。

```json
{
  "Status": 1200,
  "Result": {
    "total": 6, "per_page": 20, "current_page": 1,
    "data": [{
      "id": 1, "cgf_id": "CGF00001",
      "nickname": "沈尧", "grade": "2.6D"
    }]
  }
}
```

`id` 是 JSON 数字，就是 §6 的路径 id。`grade` 是 `2.6D` / `11.5K` 这种等级。

## 6. 账号对局 `reguser/games`

```
GET https://api.yikeweiqi.com/reguser/games/{id}/1/{page}
```

路径中的 `1` 是在线对局。`0`（综合）和 `2`（线下）会带出比赛行，其 SGF 是空的 `(;GM[1]FF[4]CA[UTF-8]SZ[19])`（GameId `29719740`、`15519701`）。下载打不开，所以只取 type 1。

不需要签名。每页 30 条，`page` 从 1。沈尧 type 1 共 131 条（第 5 页 11 条，第 6 页空）。空列表仍是 `Status=1200`。

不存在的 id 也是 `1200`，`user.name` 为 null，`grage` 为 `21.5K`。用 `name==null` 判断没有这个账号，不要用 `Status`。字段名就是 `grage`。

```json
{
  "Status": 1200,
  "Result": {
    "user": {"grage": "2.6D", "name": "沈尧"},
    "list": [{
      "GameId": 17205763, "HandsCount": 34, "BoardSize": 7,
      "BlackName": "沈尧", "BlackPlayerScore": "2.7D",
      "WhiteName": "沈知行", "WhitePlayerScore": "11.5K",
      "Result": "W+", "ResultDesc": "白胜-黑超时",
      "GameDate": "2019-12-13",
      "GameLocation": "标准区非即时",
      "TourName": "2019-12-13_标准区非即时"
    }]
  }
}
```

`GameId` 是数字，样本在 int64 内；按字符串保存，不要当浮点。`Result` 是 `B+` / `W+` / `BL` / `D` / 空。`ResultDesc` 可能是 null、`""`、`黑胜`、`49又3/4子`、`10`。`TourName` 若只是 `{GameDate}_{GameLocation}`，标题用 `GameLocation`。

`GameDate` 带或不带 `00:00:00`，都不是开赛时刻。

## 7. 账号谱 `usersgf/detail`

```
GET https://api.yikeweiqi.com/usersgf/detail?id={GameId}&type=1
```

`Status!=1200` 为失败，文案在 `Message`。`Result.Sgf` 是完整 SGF，坐标标准。`RE` 是中文，打开前换成 §8。无落子且无座子（线下空谱）不是一盘棋。

沈尧对沈知行 34 手，根节点含 `KM[7.5]`、`RU[zh]`、`BR[2.7D]`。让子写在根节点 `AB`，同时有 `HA`。9 子座子 `AB[dd][jd][pd][dj][pj][dp][jp][pp][jj]` 就是九个星位。`KM` 已经是目：分先 `7.5`，让子 `0`。

## 8. 结果句

| 原文 | `RE` |
| --- | --- |
| 黑/白 + 中盘、认输 | `B+R` / `W+R` |
| 超时 | `+T` |
| 犯规、弃权 | `+F` |
| 不计点，或只有「黑胜」 | `B+` / `W+` |
| 半目 | `0.5` |
| `N目半` | `N.5` |
| `N目`、`N点`、无单位数字 | `N`（不带 `.0`） |
| `a/b子`、`N又a/b子`、`N子` | 子数 ×2，已是目 |
| `和` / `和棋` / `D` | `0` |
| `双负` / `BL` | `Void` |
| 读不出胜者 | 空串 |

账号行先看 `Result`：`BL`、`D` 直接用上表；`ResultDesc` 以黑/白开头则只用描述；否则把 `B+` / `W+` 的胜者接到描述前面再查上表。`黑胜3/4子` → `B+1.5`，`黑胜1又3/4子` → `B+3.5`，`B+` 加 `49又3/4子` → `B+99.5`。

## 9. 签名

mirai 不签名。直播列表 `GET https://api.yikeweiqi.com/v2/golive/list` 才需要，常量在 `home.yikeweiqi.com` 的 desktop bundle（2026-09-30）：

`CheckSum = SHA1(AppSecret + Nonce + CurTime)`，`accesstoken = MD5("@1%e$5*f@3" + MD5(CurTime) + "web")`，均为小写 hex。`AppKey=3396jtzhK57XhJom`，`AppSecret=hfdSXRKm0DQyLmNXmNCNkZpjy2o5q1Hk`，`version=96813`。

```
CurTime = 1700000000123
Nonce   = 12345678
MD5(CurTime) = 8f5422e1eeeb6aae581bd979275206ce
CheckSum     = 52501c4e5494abcf371d6ea8ec68198ea5cabeeb
accesstoken  = fe7f02285cbe1c0b6501b77c37afaa3e
```

同一套头打 `v2/golive/dtl` 得到 `1403 invalid access token`。旧路径 `/golive/dtl?id=` 可用，但是直播 id，不是用户历史。

## 10. 错误与调用

- 棋手搜索无命中：空 `matches`。棋谱无命中：`code==1`。都是 HTTP 200。
- 账号接口 `Status==1200` 才是成功。用户不存在也是 1200，看 `name==null`。
- `friend/search` 匿名、`Platform=H5`、匿名对弈 JWT 都是 `1404 invalid user token`。
- 超时 25 秒，间隔 ≥ 0.5 秒。`api-new` 连打会 `Frequent Request`。

## 11. 合规

数据归弈客围棋及棋手所有。只取公开对局。用户 1 的 `hide_game_history` 为 0，列表匿名可取；没有 `value=1` 的对照，不要假设匿名列表会绕过隐藏，也不要枚举 id 或扫 `reguser/search` 的前缀。`reguser/search` 的邮箱和生日不要落盘。
