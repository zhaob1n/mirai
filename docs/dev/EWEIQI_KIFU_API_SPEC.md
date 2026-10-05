# 弈城围棋（eweiqi）棋谱查询 API Spec

匿名可取的是公开赛事目录，不是登录用户的「他的棋谱」。无官方文档或 SLA。

请求参数按 UTF-8 URL 编码。列表是 UTF-8 JSON，正文前有 BOM（`EF BB BF`），非 ASCII 用 `\uXXXX`。棋谱正文也带 BOM，但是裸 GIB，`Content-Type: application/json` 是错的，不要按 JSON 解析。

以下样本于 2026-10-03 测得（响应头 `Date: Sat, 03 Oct 2026 GMT`）。示例账号为柯洁，英文 nick `KeJie`。数值用户号 `6463429` 不在列表里，是从棋谱头像 URL 抽出的。

`https://client.eweiqi.com` 的证书主机名不匹配，一律用 `http://client.eweiqi.com`。`https://client.tygem.com` 是同一套 PHP 的韩国入口。

共用 HTTP 约定见 [野狐 spec §7](FOX_KIFU_API_SPEC.md#7-错误处理与调用约定)。

不需要 Cookie 或签名。`lang=cn` 决定分类名；不传时分类列表是韩文，id 空间不同。

## 1. 接口总览

| # | 用途 | 方法 | URL |
| --- | --- | --- | --- |
| 1 | 昵称 → 赛事目录 | GET | `http://client.eweiqi.com/gibo/gibo_load_list.php` |
| 2 | 分类目录 | GET | `http://client.eweiqi.com/gibo/gibo_load_category.php` |
| 3 | 单局 GIB | GET | `http://client.eweiqi.com/gibo/gibo_load_data.php` |

没有「昵称 → uid → 该用户最近全部对局」的匿名链。mirai 用昵称检索目录，再按目录 `id` 取谱。

## 2. 昵称检索 `gibo_load_list.php`

```
GET http://client.eweiqi.com/gibo/gibo_load_list.php?type=5&sword={name}&lang=cn
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `type` | 是 | `5` 检索。`1` 近期转播（本次 100 条，`GameType` 全是 `11`）。`4` 再加 `cate_id`。`2` 与 `3` 字节级相同，是一小份旧名局。`0`、`6`～`10` 为空 |
| `sword` | 检索时是 | 子串，同时扫 `BName` / `WName` / `BNick` / `WNick`。大小写敏感 |
| `lang` | 否 | `cn` 时分类名为中文 |
| `page` / `limit` | 否 | 不翻页。两次请求的 id 集合可以相同而顺序不同，去重用 `id` |

`sword=柯洁` 返回 336 条，每条都有柯洁，首条 `id=209557`（2026-07-30），末条 `id=140690`（2011-07-02）。`sword=柯` 返回 624 条，所以 336 不是硬上限。`sword=KeJie` 多 5 条 nick 命中。`sword=kejie` 与不存在的名字都是 3 字节 BOM，不是 `{"list":[]}`。

响应节选（线上是 `\uXXXX`，这里是解码后的）：

```json
{
  "id": "209557",
  "Title": "test",
  "BName": "柯洁", "WName": "党毅飞",
  "BNick": "KeJie", "WNick": "DangYiFei",
  "GameResult": "-999",
  "BNation": "2", "WNation": "2",
  "BRank": "35", "WRank": "35",
  "Date": "2026-07-30 17:58:44",
  "Susun": "205",
  "Category": "2996",
  "CateName": "  王中王",
  "SubCateName": "第六届",
  "chisu": "0"
}
```

`Title` 在样本里全是 `test`。赛事名用 `CateName` / `SubCateName`（常有前导空格）。列表没有数值 uid，也没有棋盘大小。`id` 是字符串，不要按整数解析再拼回去。

`lang=cn` 的分类目录有 417 条，例如 `{"cat_id":"2996","cat_name":"10.[中国] 王中王","subcat_name":"第六届"}`。`type=4&cate_id=2996` 返回 14 条，含 `209557`。用子项的 `cat_id`，不要拿不传 `lang` 时的韩文目录 id。

## 3. 单局棋谱 `gibo_load_data.php`

```
GET http://client.eweiqi.com/gibo/gibo_load_data.php?id=209557
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `id` | 是 | 列表里的目录 `id`。样本为 6 位十进制字符串 |

不要加 `mode=my`。`id=209557&mode=my` 返回 `[Error]:기보 데이터가 없습니다.`；去掉 `mode` 就是这局，85391 字节。`id=999999999` 返回同一句韩文加 `(2)`。HTTP 200 不代表成功：去掉 BOM 后以 `\HS` 开头才是谱，以 `[Error]` 开头是失败。

```
\HS
\[GAMEINFOMAIN=GBKIND:2,GTYPE:0,GCDT:0,GTIME:7200-60-5,GRLT:3,ZIPSU:0,DUM:0,GONGJE:75,TCNT:205,LINE:19,AUSZ:0\]
\[GAMEINFOSUB=GNAMEF:99,GPLCF:0,GNAME:rank game,GDATE:2026-07-30-13-31-33,GPLC:www.eweiqi.com,GCMT:姜大胖\]
\[WUSERINFO=WID:党毅飞 ,WLV:35,WNICK:DangYiFei,WNCD:2,WAID:0,WIMG:http://images.eweiqi.com/wuser/1/558/736/photo/thumbs/tu000_1558736.jpg\]
\[BUSERINFO=BID:柯洁 ,BLV:35,BNICK:KeJie,BNCD:2,BAID:0,BIMG:http://images.eweiqi.com/wuser/6/463/429/photo/thumbs/tu000_6463429.jpg\]
\HE
\GS
INI 0 1 0 &4
STO 0 2 1 15 3
STO 0 3 2 3 3
\GE
```

姓名尾部常有空格。`GNAME` 样本全是 `rank game`，不是赛事名。`BAID` / `WAID` 样本全是 `0`。用户号在头像 URL 的 `tu000_` 与 `.jpg` 之间（柯洁 `6463429`）。

## 4. 字段字典

列表字段全是字符串。

| 字段 | 语义 |
| --- | --- |
| `id` | 目录主键。与 §6 用户文件里的 `id` 不是同一空间 |
| `BName` / `WName` | 中文显示名，空则回退 nick。mirai 用它们而不用棋谱的 `BID` / `WID`：棋谱里的名字是上传者代码页的原始字节，服务器按另一种代码页转成 UTF-8。`209302` 棋谱是 `죔禱붐` / `온썅`（GBK 被当成 CP949，即 廖元赫 / 柯洁），`196734` 棋谱是 `脚柳辑`（CP949 被当成 GBK，即 신진서），列表分别是 `廖元赫` / `柯洁` 与 `申眞揟` |
| `BRank` / `WRank` | 段位码，见 §4.1 |
| `BNation` / `WNation` | `2` 中国、`1` 日本、`0` 韩国。`4` 出现过一次，未对照 |
| `GameResult` | 见 §4.2。`0` 不是和棋 |
| `Susun` | 手数，与棋谱 `TCNT` 一致 |
| `Date` | `yyyy-MM-dd HH:mm:ss`，无时区。比棋谱 `GDATE` 晚数小时，见 §5.2 |
| `CateName` / `SubCateName` | 赛事名，trim 后用空格接起来 |
| `chisu` | 样本全是 `0`。让子没有活样本 |

棋谱头：`GTIME` 是 `主时间秒-读秒秒-读秒次数`（`7200-60-5`；另一局 `600-30-1` 与 `GAMETAG` 的 `T30-1-600` 对得上）。`GONGJE` 与 `GAMETAG` 的 `G` 是十分之一目，`75` = 7.5 目。`LINE` 是路数，抽到的五局全是 `19`。`GDATE` 是开局 `yyyy-MM-dd-HH-mm-ss`。`GBKIND` 样本全是 `2`（qGo 注释：中国服务器）。

### 4.1 段位编码

与 qGo `tygemconnection.cpp` 一致，并用在役职业棋手核对：

| 码 | 身份 |
| --- | --- |
| `>= 27` | 职业，段数 = 码 − 26。`35` 职业九段，`27` 职业初段 |
| `18`～`26` | 业余段，段数 = 码 − 17。`26` 业余九段 |
| `0`～`17` | 级位，级 = 18 − 码。`17` 是 1 级，`0` 是 18 级 |

活样本：柯洁、党毅飞、申眞揟、范廷钰的 `35` 为职业九段；陈梓健 `33` 为职业七段；2014-08-03 朴沧溟 `26` 对柯洁 `30`。没有级位样本。mirai 写成 `P9` / `9d` / `18k`。把 `18`～`26` 当成职业是错的。

### 4.2 胜负编码

负号黑胜，正号白胜。`ZIPSU` 是十分之一目。解说写的是「目」，不是「子」。`KM` 用 `GONGJE/10`，不要再乘 2。

| 列表 `GameResult` | `GRLT` | `ZIPSU` | 解说 | SGF `RE` |
| --- | --- | --- | --- | --- |
| `-999` | `3` | `0` | 黑 中盘胜（`209557`） | `B+R` |
| `999` | `4` | `0` | 白 中盘胜（`209554`） | `W+R` |
| `-888` | `7` | `0` | 黑 时间胜（`201652`） | `B+T` |
| `-5.5` | `0` | `55` | 黑 5目半胜（`196734`） | `B+5.5` |
| `1.5` | `1` | `15` | 白 1目半胜（`186782`） | `W+1.5` |

`GameResult=0` 不是和棋。`185399` 列表为 `0`，棋谱却是 `GRLT=4`、解说「白 中盘胜」。列表为 `0` 时以棋谱为准。`+888` 本次没见到。`[INFERENCE]` 白超时应为列表 `888`、`GRLT=8`，与 `-888` / `7` 对称，未实测。

## 5. GIB 与落子

主线只取 `\GS` 与 `\GE` 之间的 `STO`。`209557` 这段恰好 205 行，等于 `TCNT`。`\GE` 之后是解说。

```
STO 0 {序号} {颜色} {x} {y}
```

颜色 `1` 黑、`2` 白。序号从 2 起。`x`、`y` 为 0～18，**0 是棋盘边缘，不是 pass**（`209557` 主线有 3 手坐标为 0）。坐标已经是上到下：`y = 0` 是顶行，与 mirai 的 `Point` 相同，不要翻转。`209557` 前五手是 `(15,3) (3,3) (15,16) (3,16) (2,2)`，SGF 为 `B[pd];W[dd];B[pq];W[dq];B[cc]`（弈客同一局的抄本以此开头）。出界的坐标按失败处理。

`SKI` 是 pass，轮到「上一手不是自己」的一方：无让子时先黑，让子后先白。行内的颜色字节不可靠。qGo 还把 `WIT` 当悔棋、`SUR` 当认输、`REM` 当终局提子；这五局主线里都没出现。

让子：`INI 0 1 {n}` 的第 4 个字段（零基索引 3）在 `2`～`9` 时，按 Sabaki `gib.js` 的 `getHandicapPlacement(n, {tygem:true})` 放座子，角的顺序与 mirai 的 `fixed_handicap` 相同。这是源码对照，不是活样本：本次 `chisu` 全是 `0`，主线 `INI` 的这个字段全是 `0`。

`GDATE` 取前 10 字节作 `yyyy-MM-dd`；不足 10 字节或截断 UTF-8 字符时不写日期。

### 5.1 解说与变化图

```
\RS
\[REFSUSUN=3\]
\[REFEXPLAIN=
白点三三
\]
\RE
```

`REFSUSUN` 是解说时已下的手数，不是 `STO` 的序号：`209557` 在 `REFSUSUN=31` 的变化图先重放实战 31 手再接变化，`73` 处的解说问的正是第 73 手，终局总结与「黑 中盘胜」在 `205`（共 205 手）。所以 `n` 记在第 `n` 手（pass 也算一手），`0` 或没有序号记在根节点，超过总手数记在最后一手。同一手的多块按出现顺序用换行接起来。

变化图是另一类块，整图重放而不是增量：

```
\RS
\[REFSUSUN=31\]
\[REFGIBO=
STO 0 2 1 15 3
...
\]
\RE
```

含 `REFGIBO=` 的块整块跳过。`209557` 的 `\GE` 之后还有 3378 行 `STO`，都在这些图里。

### 5.2 时间

`GDATE` 是开局。列表 `Date` 更晚：`209557` 为 13:31:33 对 17:58:44，`209554` 相差也约 4.5 小时。`[INFERENCE]` 列表 `Date` 是结束时间。两处都没有时区。`[INFERENCE]` 为东八区：主机在弈城，时刻落在下午直播时段；没有用 UTC 对时。

## 6. 登录态接口（mirai 不实现）

当前网页客户端（`mobile.eweiqi.com` `index.js` 1.0.0.148，Last-Modified 2026-09-21）把 `USE_RSA_SHIELD` 设为真。用户自己的棋谱走：

| 步骤 | 接口 |
| --- | --- |
| 取 nonce | `POST /gibo/gibo_nonce.php` → `{"nonce","expiresIn":60}` |
| 用户号 | `POST /gibo/gibo_load_user_new.php` |
| 用户棋谱文件 | `POST /gibo/gibo_load_user_file_new.php` |
| 单局 | `POST /gibo/gibo_load_data_new.php` |

正文是 `enc_data=` 加上 RSA-OAEP（SHA-1，MGF1 也是 SHA-1）加密后的十六进制。公钥在该 `index.js` 的 `Ran` 里，SPKI DER 的 SHA-256 为 `7b27e0d4b19ce5ef9ba3a4e451e703e1b4bab2650f7a4e9cc3f4412e0906f10b`。明文是业务 JSON 再并入 `nonce` 与 `timestamp = floor(unix 秒 / 60)`。

匿名打不通，2026-10-03 实测：无 `enc_data` 为 403 `Missing enc_data.`；空口令为 `Missing credentials.`；`uid=KeJie` 加错误口令为 `Authentication failed.`。旧的 `gibo_load_user.php` 对明文 nick 返回空 BOM。`gibo_load_user_file.php?usernum=6463429` 只吐 2014 年 1 局，其 9 位 `id` 交给 §3 得到 `(2)`，下不下来。要某人的近期公开谱，用 §2。

## 7. 错误处理与调用约定

- 目录：先去 BOM。空 BOM 是没查到。非空正文须能按目录结构解析为 JSON；mirai 将缺失的 `list` 当作空列表，存在但不是数组时按失败处理。
- 棋谱：去 BOM 后 `\HS` 为成功，`[Error]` 为失败。`(2)` 是 id 不在目录。没有 `(2)` 的「没有数据」多半是 `mode=my`。
- `id` 按字符串保存。样本远小于 2^53，仍不要解析后重新拼接。
- 列表顺序不稳定，入库以 `id` 去重。

## 8. 最小调用示例

```bash
curl -s -A "$UA" --get --data-urlencode 'sword=柯洁' \
  'http://client.eweiqi.com/gibo/gibo_load_list.php?type=5&lang=cn'

curl -s -A "$UA" \
  'http://client.eweiqi.com/gibo/gibo_load_data.php?id=209557'
```

## 9. 合规提醒

数据归弈城围棋及棋手所有。只取公开目录，不要对 `*_new.php` 猜口令，不要批量扫 `sword` 或全部分类。
