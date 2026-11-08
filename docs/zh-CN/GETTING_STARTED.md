# 快速开始

[文档目录](INDEX.md)

## 选择运行方式

已提交 runtime 的版本为 v1.12.5。使用 `make build` 构建当前服务／前端，
或使用同版本候选包。

WebSA 运行于 Linux。仓库的 `runtime/` 包含 Linux x86_64/glibc 服务、SDK 适配器和前端。
已记录的捆绑版本要求 glibc 2.44、GLIBCXX 3.4.30、CXXABI 1.3.15 和 `libfdk-aac.so.2`；
它不是静态或通用跨发行版分发。旧系统或其他架构请按照[开发指南](DEVELOPMENT.md)重新构建。

硬件模式需要 SAN-45／SAN-60／SAN-90，以及单独取得、安装于 `/opt/htraapi` 的 HAROGIC SDK
及其共享库依赖。专有 SDK 不包含在仓库中。模拟模式无需分析仪或 SDK，但需要仓库的
`tests/fixtures/`。启动脚本依赖 Bash、coreutils、util-linux（`flock`）、`/proc` 和 curl。
运行捆绑版本不需要 Rust、Node 或 Python。

## 启动

在仓库根目录执行：

```bash
./run.sh --fake                 # 无硬件的合成输入
./status.sh
./stop.sh
./run.sh                        # 已安装 SDK 的真实硬件
```

打开 **http://127.0.0.1:8080**。启动前关闭占用分析仪的其他程序。
脚本启动属于当前检出的后台进程，不安装系统服务。模拟信号用于演示控制和已知样本，不能证明
射频灵敏度或持续运行容量。

启动独立模拟实例：

```bash
WEBSA_INSTANCE=demo WEBSA_PORT=8099 ./run.sh --fake
./stop.sh --instance demo
```

## 选择文件与网络访问

启动脚本优先使用 `backend/target/release/websad` 和 `frontend/dist/`，缺失时使用
`runtime/linux-x86_64/websad` 和 `runtime/frontend/dist/`。`WEBSA_BIN` 和 `WEBSA_STATIC`
可覆盖选择；静态目录参数指包含 `dist/` 的目录。SDK 适配器按 `WEBSA_RUST_IQS_SHIM`、
用户已安装版本、runtime 版本的顺序选择。`WEBSA_FIXTURES` 可指定样本目录。

默认只监听本机。`WEBSA_HOST=0.0.0.0 ./run.sh` 允许局域网访问 8080 端口；产品 HTTP 接口
没有身份认证。WebSocket 要求同源或来源位于 `WEBSA_ALLOWED_ORIGINS` 白名单。
通过 `WEBSA_PORT` 修改端口。

## 更新与首次使用

先停止实例，再构建或替换同一版本的服务／前端／适配器，然后重新启动并运行 `./status.sh`。
重新构建不会自动更新已运行的进程。源码构建和打包命令见[开发指南](DEVELOPMENT.md)。

先打开频谱视图，切换至 SDR，选择采集范围内的频率并启用监听。
[接收操作](USER_GUIDE.md)解释信道选择、本地音频和录制；库缺失、无声或设备报错见
[运行与排障](OPERATIONS.md)。
