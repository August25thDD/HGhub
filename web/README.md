# 自托管网页版

本目录是独立 axum HTTP 服务，直接编译 `src-tauri/src/` 中的 API、网络策略、会话、媒体和解密模块，不依赖 Tauri、GTK 或桌面环境。前端以 `VITE_HGHUB_WEB=1` 构建，通过 `src/bridge.ts` 将调用发送到同源 `/api/invoke/{cmd}`。`/media/img`、`/media/hls`、`/media/seg` 复用 Rust 媒体层，HLS 的所有资源地址都改写到同源代理。

所有页面、静态资源、业务 API 和媒体都受账号及密码保护，只有登录表单允许匿名访问。服务校验 Host 和写请求 Origin，使用 Argon2id 校验密码，Cookie 为 HttpOnly / SameSite=Strict；配置 HTTPS origin 时同时设置 Secure。会话过期或服务重启需要重新登录，设置页可退出网页版。不同浏览器的上游账号凭据按会话隔离，不共享或持久化到服务账号。

构建与测试：

```sh
pnpm install --frozen-lockfile
pnpm build:web
cargo test --locked --manifest-path web/Cargo.toml
cargo build --release --locked --manifest-path web/Cargo.toml
bash scripts/install-web-service.sh
python3 scripts/verify-web.py
node scripts/verify-web-browser.cjs
```

浏览器验证需要已安装 Playwright Chromium/WebKit 及运行库。`HGHUB_PLAYWRIGHT_MODULE` 可指定已有的 `playwright-core` 安装路径。该脚本验证实际登录、页面渲染、真实 feed/详情、封面与 HLS 代理，以及视频解码；不能代替实体 iPhone 和公网验收。

安装脚本默认生成随机访问密码，并只在 `.clawbench/private/web-password.txt` 中保存；运行服务读取 `.clawbench/private/web-password.hash`，不会把密码传到命令参数或日志。可通过安全终端将自己的密码写入私有文件，再使用二进制的 `--hash-password` 标准输入模式生成散列。私有目录、日志、安装包和运行数据均在 Git 忽略范围内。

服务配置位于 `.clawbench/private/web.env`：

| 参数 | 初始值 / 用途 |
| --- | --- |
| `HGHUB_USERNAME` | 私人登录账号，初始值 `hghub` |
| `HGHUB_LISTEN` | `127.0.0.1:8900`，先只监听本机 |
| `HGHUB_PUBLIC_ORIGIN` | `http://127.0.0.1:8900`，须与实际浏览器访问的 origin 完全一致 |
| `HGHUB_PASSWORD_HASH_FILE` | Argon2 密码散列文件的绝对路径 |
| `HGHUB_DATA_DIR` | 本机线路设置、缓存诊断状态目录 |
| `HGHUB_DIST` | 以网页版模式构建的 `dist` 目录 |
| `HGHUB_STRICT` | 固定 `1`，服务器不允许浏览器关闭网络保护 |

systemd 单元为当前账号的 `hghub-web.service`，安装脚本启用并启动它。主机须启用当前账号的 linger，才能在未登录时随开机启动。脚本回读 `enabled`、`active` 和 `Linger`，不会擅自以 root 修改主机配置。

部署顺序：先核对云账号注册邮箱、租户和已有实例，在本机验通以上服务和真实媒体链路，然后配置公网入口。使用 Cloudflare Tunnel 时保持 `127.0.0.1:8900` 监听，无需开放云安全列表的 8900 入站端口；直连或自有反向代理方案再按实际需要配置网络规则。公网验收须通过公网域名执行验证脚本：匿名被挡回登录页；账号密码登录后真实 feed 和详情正常；封面、HLS 清单、密钥及分片返回成功。**本机通过不代表公网部署完成。**

Cloudflare Tunnel 部署配置约定：

- 创建专用 named tunnel，将公开子域名路由到 `http://127.0.0.1:8900`，最后一条 ingress 使用 `http_status:404`。
- 本机配置存于 `.clawbench/private/cloudflared.yml`，隧道凭据仅由 `cloudflared` 保存在当前账号的私有目录中，不进入 Git。
- 将 `HGHUB_PUBLIC_ORIGIN` 设置为实际 HTTPS origin，`originRequest.httpHostHeader` 与该公网主机名保持一致，然后重启 `hghub-web.service`。
- 用当前账号的 `hghub-tunnel.service` 常驻运行隧道，启用开机自启；与 `hghub-web.service` 一起回读 `active`、`enabled` 和当前账号的 `Linger=yes`。
- 访问凭据保存在 `.clawbench/private/web-login.txt`，公网验收记录使用 `.clawbench/reports/web-public.json`；浏览器报告记录桌面与 iPhone WebKit 模拟验证，不能当作实体 iPhone 实测。
- Workers 发布权限不等于 Tunnel/DNS 管理权限。先实测现有授权；需要补充时使用官方 `cloudflared tunnel login` 浏览器流程，不传抄令牌。
- 验收客户端默认使用浏览器 User-Agent，以兼容 Cloudflare 浏览器完整性检查。可用 `--user-agent` 显式指定；应用的账号密码校验、同源校验和 Cookie 安全属性仍须全部通过。

例如公网 origin 已配置为自己的 HTTPS 域名后，从外部环境运行：

```sh
python3 scripts/verify-web.py --origin https://your-private-host.example --password-file /secure/web-password.txt --report external-check.json
```

不要将真实上游接口、播放地址、响应正文或密钥提交到仓库。许可证及个人非商业使用边界沿用项目根目录的说明。
