# 哔哩哔哩登录与持续登录态研究（纯 CDP 实测）

> **研究方法声明**：本文所有结论均来自对真实站点的 CDP（Chrome DevTools Protocol）被动观察 ——
> 包括浏览器实际发出的请求/响应、Set-Cookie、以及站点自己下发的压缩 JS 源码切片（miniLogin / token-iframe / 主站 bundle）。
> 未参考任何第三方 API 文档。文中代码片段均为官方 JS 原文摘录（仅做最小格式化）。
> 研究日期：2026-11-15，Chrome 157，登录测试账号实测。

---

## 1. 结论总览：什么是"登录态"

| 组成 | 载体 | 有效期（实测） | 说明 |
|---|---|---|---|
| **会话令牌** | Cookie `SESSDATA`（HttpOnly，222B） | **180 天**（2026-11-15 → 2027-05-14） | 核心 token，携带它即可被识别为登录 |
| **CSRF** | Cookie `bili_jct`（32B，非 HttpOnly） | 180 天 | 调用写操作/部分 GET 必须作为 `csrf` 参数回传 |
| **用户 ID** | Cookie `DedeUserID` + `DedeUserID__ckMd5` | 180 天 | mid 与其校验串 |
| **刷新令牌** | `localStorage["ac_time_value"]`（32B） | 随每次续期轮换 | **refresh_token，不在 Cookie 里**，用于 180 天后续期 |
| **风控票据** | Cookie `bili_ticket` + `bili_ticket_expires` | 3 天 | 部分接口需要；生成算法见 §5 |
| **设备指纹** | Cookie `buvid3` / `buvid4` / `buvid_fp` / `b_nut` / `_uuid` | 数月~1 年 | 建议持久化并在每次请求携带 |

> 实测登录成功时间戳与 Cookie 过期时间吻合：过期 = 登录时间 + 180 天。
> Cookie 同时镜像写入 `.biligame.com`（crossDomain 同步），对 API 调用无影响。

**最小可用集合**：`SESSDATA` + `bili_jct` + `DedeUserID` + `buvid3` 即可让大多数 `api.bilibili.com` 接口返回 `isLogin: true`。

---

## 2. 扫码登录流程（网页端，全部实测）

### 2.1 生成二维码

```
GET https://passport.bilibili.com/x/passport-login/web/qrcode/generate
→ data: { url, qrcode_key }
```

- `data.url` 是二维码内容（`https://passport.bilibili.com/h5-app/passport/login/scan?...&qrcode_key=...`），
  前端把该 URL 本地渲染成 PNG（页面里是 `data:image/png;base64` 的 `<img>`，无第三方图床）。
- `qrcode_key`（32 hex）是本次登录会话的轮询凭证。

### 2.2 轮询登录状态

实测页面（主站 header 面板）每 ~2 秒发起：

```
GET https://passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key=<key>
    &source=main-fe-header&web_location=333.1007
    &x-bili-locale-json={...}&x-bili-redirect=1
    &x-bili-device-req-json={"platform":"web","device":"pc","spmid":"333.1007","mobi_app":"web_cn"}
    &b_ret=<风控环境token>
```

`data.code` 状态机（实测捕获）：

| code | 含义 |
|---|---|
| 86101 | 未扫码 |
| 86090 | 已扫码，等待手机确认 |
| **0** | **成功** → `data.url` 内含 `SESSDATA`/`bili_jct`/`DedeUserID`/`DedeUserID__ckMd5`/`Expires` |
| 86038 | 二维码失效（过期**或已被消费**；成功后立刻再 poll 也会得到 86038） |

成功后浏览器跳转 `data.url`（biligame crossDomain 页），它把 Cookie 写到 `.bilibili.com` 与 `.biligame.com` 两个域。轮询响应 JSON 里同样带出 `refresh_token` 与 `timestamp`（token-iframe 组件 `S` 的数据模型 `{type, url, refresh_token, timestamp}`）。

### 2.3 登录成功后的 Token 归档（对应端到端实测）

`miniLogin.umd.min.js` 原文（摘）：

```js
var w = "https://www.bilibili.com/correspond/0/",
    k = "https://s1.hdslb.com/bfs/static/jinkela/long/wasm/wasm_ras_umd.js";
setTokenData: function(e) {           // e = {type, url, refresh_token|token, timestamp}
  this.refresh_token = e.refresh_token || e.token;
  this.timestamp = e.timestamp;
  // 非 Safari: hashPath = wasmEncrypt(hex("set_" + timestamp))
  this.getEncryptCode("set_" + this.timestamp)  →  this.url = w + <hashPath>
  // 创建 iframe (id="token_iframe") 指向该 url，并向其 postMessage({longToken: refresh_token})
}
getEncryptCode: function(e) {
  return window.wasmInit.encrypt({ data: this.convertToHex(e), digest: "SHA256" });
}
convertToHex: 逐字符 charCodeAt(n).toString(16)   // 无补零
```

iframe 页 `/correspond/0/<hashPath>`（token-iframe chunk 900，组件名 `TokenSet`）原文（摘）：

```js
276: var o = "ac_time_value";          // localStorage 键名
setLongTokenByPostMsg: function(e) {   // 监听父页面 postMessage
  if (-1 !== e.origin.indexOf(".bilibili.com") && e.data && e.data.longToken) {
    localStorage.setItem("ac_time_value", e.data.longToken);   // ★ refresh_token 落库
  }
}
```

实测：登录完成后 `localStorage.ac_time_value` = 32 字符 refresh_token。✅

---

## 3. 持续登录：Cookie 续期链路（✅ 已实现：`crates/bili-plugin-login/src/refresh.rs`）

### 3.1 检测是否需要刷新（GET，无害，实测通过）

```
GET https://passport.bilibili.com/x/passport-login/web/cookie/info?csrf=<bili_jct>
→ data: { refresh: true|false, timestamp: <服务器毫秒时间戳> }
```

主站 header 挂载时若已登录即调用（index bundle 原文，摘）：

```js
const A = async () => {
  const { data: B } = await vj();      // GET cookie/info
  if (B?.refresh) {
    await ea("//s1.hdslb.com/bfs/static/jinkela/long/wasm/wasm_ras_umd.js");
    const N = B.timestamp, U = "refresh";
    const H = window.wasmInit; await H.default();
    const z = H.encrypt({ data: ij(U + "_" + N), digest: "SHA256" });
    g.value = "//www.bilibili.com/correspond/1/" + z;    // iframe src
  }
};
```

### 3.2 取刷新凭证（SSR 页）

`GET https://www.bilibili.com/correspond/1/<hashPath>`，其中
`hashPath = wasmEncrypt(hex("refresh_" + timestamp))`（**已现场计算并请求成功**，实测返回 1110B HTML）。

该 SSR 页面内嵌两个隐藏 DOM（token-iframe chunk 895，组件名 `TokenRefresh`）：

- `#1-name` 的 innerText = **refresh_csrf**（实测 32 字符）
- `#1-value` 的 innerText = refresh_token（实测为空时回退 `localStorage.ac_time_value`）

chunk 895 原文（摘）：

```js
n = getRefreshCsrf:  var e = getDomText("1-name");  e && (r.value = e);
i = getLongToken:    var e = getDomText("1-value");
                     e ? (localStorage.setItem("ac_time_value", e), t.value = e)
                       : t.value = localStorage.getItem("ac_time_value");
```

### 3.3 三步刷新 API（均为 POST，`Content-Type: application/x-www-form-urlencoded`）

chunk 895 原文（摘，`eQ = "//passport.bilibili.com"`）：

```js
s = handleRefreshCookie:                       // ① 换发新 Cookie + 新 refresh_token
  POST eQ + "/x/passport-login/web/cookie/refresh"
  data: { refresh_csrf: <r.value>, source: "main_web", refresh_token: <t.value> }
  成功 → localStorage.setItem("ac_time_value", resp.data.refresh_token)   // 立即轮换
       → u() → c()

u = handleSetSSO:                              // ② 同步登录态到 SSO 各域
  POST eQ + "/x/passport-login/web/sso/list"   // data: {}
  resp.data.sso[] → 对每个 url: POST <url>  data:{} （写该域 Cookie）

c = handleConfirmRefresh:                      // ③ 确认（此后旧 SESSDATA 失效）
  POST eQ + "/x/passport-login/web/confirm/refresh"
  data: { refresh_token: <旧 token> }
```

> ⚠️ ①成功后浏览器会收到新的 `SESSDATA` 等 Set-Cookie（新旧短期并存）；**confirm 之后旧 Cookie 作废**。
> 实现 Rust 版续期时必须走完 ①②③ 并保存新 `SESSDATA`/`bili_jct`/`ac_time_value`，中途失败不要 confirm。

### 3.4 correspond path 加密器（已实现原生复用，不再依赖 wasm）

**wasm 鉴定结论**（`wasm_rsa_encrypt_bg.wasm`，343KB）：

- producers section：`language: Rust`，`rustc 1.60.0 (2022-04-04)`，`wasm-bindgen 0.2.58`
- panic 路径暴露依赖树：**rsa crate（git 版，oaep.rs）**、`num-bigint-dig 0.6.0`、
  `serde_json 1.0.45`（输入 JSON `{data, digest}`）、`hex 0.4.0`、`getrandom 0.1.14`
- **算法实锤：RSA-1024 + OAEP(SHA256)**（getRandomValues = OAEP 随机种子，密文非确定性）
- 公钥硬编码在 data section（256 hex 模数）：

```
n = cb81dd8e02470656da04dd38544446e2a3412051cfe9adc6a330a5ef90228509
    684960970b91c3360ca29c49e1690ff8fa068cb9dfc6179d1e9585cb9424e847
    db1ef59f33e37dd4dca8ccfb7631ee9b4a92640d00c8204300152a0ab7cd8028
    89d3445aec69918fe6022b534912e7b095be3424dad1ba81145e969b533181f1
e = 65537
```

**明文格式（双假设对照实验定案）**：
- ✔ 正确：`"refresh_<ts>"` / `"set_<ts>"` 的**原始 ASCII 字节**直接作 OAEP 明文
- ✘ 错误：hex 编码串的 ASCII 字节（服务器 404 拒绝）
- miniLogin 的 `convertToHex` 只是把明文 hex 字符串传给 wasm，wasm 内部 `hex::decode` 后加密

**原生 Rust 复用已验证**：`rsa` crate（0.9）+ `sha2` 提取公钥实现 OAEP-SHA256，
native path 经服务器解密通过（`GET /correspond/1/<native_path>` 返回含 `#1-name`
refresh_csrf 与 `#1-value` refresh_token 的 SSR 页）。实现见
`crates/bili-plugin-login/src/correspond.rs`——**客户端无需携带 wasm 文件**。

Node 端验证脚本与 wasm 测试向量存于 `.qaqh/research/`（wasm-test-vector.json）。

---

## 4. WBI 签名（`w_rid`/`wts`，挖自主站 index bundle）

原文（摘）：

```js
Nw = function(t) {                       // mixin：64 字符按固定置换表重排，取前 32
  var n = [46,47,18,2,53,8,23,32,15,50,10,31,58,3,45,35,27,43,5,49,33,9,42,19,29,
           28,14,39,12,38,41,13,37,48,7,16,24,55,40,61,26,17,0,1,60,51,30,4,22,25,
           54,21,56,59,6,63,57,62,11,36,20,34,44,52];
  return n.map(a => t.charAt(a)).join("").slice(0, 32);
};
jw = function(params, opts) {
  // key 来源优先级: opts.wbiImgKey/wbiSubKey → localStorage["wbi_img_urls"]
  //   localStorage 值形如 "<img_url>-<sub_url>"，取 URL 文件名(xw)为 key
  // 兜底: 内置 I2 = {wbiImgKey:"d569546b86c252:db:9bc7e99c5d71e5",
  //                 wbiSubKey:"557251g796:g54:f:ee94g8fg969e2de"}（T2 = 逐字符-1 解码）
  var f = Nw(r + a);                     // 64 位拼接 → 置换 → 32 位 mixin key
  var h = Math.round(Date.now() / 1000); // wts
  var v = {...params, wts: h};
  // keys 排序；值若为字符串先删除字符类 [!'()*]；encodeURIComponent
  var b = <排序拼接后的 query>;
  var C = Bw(b + f);                     // Bw = md5
  return {...params, w_rid: C, wts: h.toString()};
};
// 另: window._render_data_?.access_id 存在时会注入参数 w_webid
```

wbi 密钥的两个来源（均已实测）：

1. `GET /x/web-interface/nav` → `data.wbi_img.img_url / sub_url`（文件名即 img_key/sub_key）
2. `localStorage["wbi_img_urls"]` = `img_url + "-" + sub_url`（由 GenWebTicket 响应的 `data.nav` 写入，见 §5）

---

## 5. bili_ticket（3 天风控票据，挖自 token-iframe chunk 895）

原文（摘，常量已确认）：

```js
eP = "bili_ticket"; eN = "bili_ticket_expires"; eA = "wbi_img_urls";

eH = async () => {
  var t = Math.floor(Date.now() / 1000);
  var r = "YhxToH[2q";                              // 混淆密钥
  // ea(s): 逐字符 String.fromCharCode(charCodeAt(s) - 1)  →  实际密钥 "XgwSnGZ1p"
  var n = CryptoJS.HmacSHA256("ts" + t, ea(r));
  var o = CryptoJS.enc.Hex.stringify(n);
  return en("//api.bilibili.com/bapis/bilibili.api.ticket.v1.Ticket/GenWebTicket",
    { key_id: "ec02", hexsign: o, "context[ts]": t, csrf: cookie("bili_jct") });
};

// 续签判定 eD(): cookie bili_ticket_expires(秒) - now <= 30 分钟 或已过期 → 重新生成
eB = ({ticket, nav, ttl, created_at}) => {
  setCookie("bili_ticket", ticket, ttl);                          // domain=.bilibili.com
  setCookie("bili_ticket_expires", String(created_at + ttl), ttl);
  localStorage["wbi_img_urls"] = nav.img + "-" + nav.sub;         // 顺带更新 wbi 密钥
};
```

实测观察：未登录首页加载即生成 `bili_ticket`（156B，3 天后过期），说明生成**不强制要求登录态**（`csrf` 取不到时可能可省略/为空——未实测）。

---

## 6. 登出 / 密码 / 短信登录

### 6.1 登出（主站 header 真实实现，bundle 挖出）

```js
{key:"logout", value:function(k,T){
  var R={biliCSRF: getCookie("bili_jct")};
  T&&(R.gourl=T);
  h({method:"POST", url:"//passport.bilibili.com/login/exit/v2",
     headers:{"Content-type":"application/x-www-form-urlencoded"}, data:R})
```

即：`POST https://passport.bilibili.com/login/exit/v2`，form `{biliCSRF: <bili_jct>}`。
已在 `QrAuthPlugin::logout` 实现（尽力调用 + 本地清除）。

### 6.2 密码 / 短信登录（仅端点确认，未走通）

`miniLogin.umd.min.js` 中确认的端点（都走 geetest4 验证码体系）：

```
GET  /x/passport-login/captcha           (?support_captcha_types=geetest4)
POST /x/passport-login/web/login         （密码，需 geetest4 challenge；密码加密方式未挖）
GET  /x/passport-login/web/country
POST /x/passport-login/web/sms/send
GET  /x/passport-login/web/sns/state/generate   （QQ/微信等 SNS 登录）
GET  /x/passport-login/web/sns/authorize/url
```

第三方客户端建议只做扫码登录（免验证码、全流程无人工干预）。

---

## 7. Rust 实现路线（biliplay-rs）

1. **依赖**：`reqwest`（cookie store 开启）+ `hmac`/`sha2`（bili_ticket）+ `md5`（wbi）+ `serde_json`。
2. **登录**：实现 generate + poll（§2），二维码用 `qrcode` crate 本地渲染；poll 间隔 2s；code=0 后从 `data.url` 解析查询参数拿第一组 Cookie，同时保留 `refresh_token`。
3. **续期**（§3）：`cookie/info` → 返回 `refresh:true` 时走 correspond/1 → cookie/refresh → sso/list → confirm/refresh。需要 wasm：可用 `wasmtime` 加载官方 `wasm_ras_umd.js`（抓包保存），或逆向其 SHA256 逻辑后原生实现（研究目录留有该 wasm 的引用与测试向量）。
4. **请求封装**：
   - 通用头：UA、`Referer` 相关接口按页面来源补；
   - 需要 wbi 的接口（URL 含 `/wbi/`）按 §4 签名；
   - 高风险接口携带 `bili_ticket`（§5）；
   - 所有请求带 `buvid3/buvid4` 等设备 Cookie。
5. **存储**：把 Cookie + `ac_time_value` 持久化（见 `.qaqh/secrets/bili-cookies.json` 结构），启动时校验 `cookie/info`，`refresh:false` 直接可用。

---

## 8. 研究产物清单

| 文件 | 内容 |
|---|---|
| `.qaqh/secrets/bili-cookies.json` | 完整 Cookie（39 枚，含 HttpOnly 全值）⚠️ 敏感，勿提交 git |
| `crates/bili-plugin-login/src/correspond.rs` | correspond path 原生实现（RSA-OAEP-SHA256，公钥已提取，服务器验证通过） |
| `crates/bili-plugin-login/src/refresh.rs` | Cookie 续期链路（info→correspond/1→refresh→sso→confirm，官方同序） |
| `.qaqh/research/wasm-test-vector.json` | 官方 wasm 加密测试向量（模数/输入/密文） |
| `.qaqh/research/wasm_rsa_encrypt_bg.wasm` | 官方 wasm 本体（已可抛弃，留作对照） |
| `.qaqh/research/cdp-*.mjs` | CDP 工具集（capture/qrcodelogin/eval/navigate/shot/watch/dumpcookies） |
| `.qaqh/research/chunk895.js` / `index-bundle.js` | token-iframe 刷新组件与主站 bundle（§3/§4/§5 证据） |

## 9. 实现校准（bili-plugin-login 首版实测修正）

实现扫码登录插件时，以下细节与最初推断不同（均经真实登录实测修正）：

1. **poll 成功响应的 `data.url` 查询参数里没有 SESSDATA**——Cookie 通过
   `Set-Cookie` 响应头下发（poll 响应自身 + `data.url` 跳转链每跳）。
   实现取 Cookie 的顺序：poll 响应 Set-Cookie → （缺 SESSDATA 时）手动跟随
   data.url 跳转链收集每跳 Set-Cookie → url 查询参数仅作字段兜底。
2. **URL 查询参数（gourl/Expires 等）不是 Cookie**，混入 Cookie 头会污染请求
   （gourl 值含 `&?=` 等字符）。cookies 表只收 Set-Cookie 来源。
3. `finger/spi`（buvid3/buvid4）确实可在未登录时调用，登录后补设备指纹可用。
4. poll JSON 成功响应里带 `refresh_token`（32B）——正是续期用的 ac_time_value，
   无需再走 correspond/0 存取流程即可拿到。
5. `nav` 响应的 `isLogin` 是 **camelCase**（snake_case + serde default 会静默变 false）。

## 10. 未完全确认项（诚实清单）

- `qrcode/generate` 请求本身的完整 query（页面在观察窗口开启前已发出；仅从 JS 确认路径与入参结构）。
- GenWebTicket 的 `csrf` 在未登录时是否可省略。
- `w_webid`（`_render_data_.access_id`）在普通页面的取值与必要性。
- 密码登录的密码加密方式与 geetest4 对接（未实施）。
- wasm_encrypt 是否依赖时间戳新鲜度窗口（同一 ts 多次计算结果一致，但服务器侧校验窗口未测）。
