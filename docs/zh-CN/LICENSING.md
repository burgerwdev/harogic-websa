# 许可落实与核对记录

本记录基于 2026-11-08 的本地检查，基准修订为 `ee0c0b8c`。
新授权只适用于授权人有权处置的自有版权。请合并阅读[适用范围](../../LICENSING.md)、
[标准 AGPL 正文](../../LICENSE)、[第 7 条附加许可](../../LICENSE-EXCEPTION)和
[第三方清单](../../THIRD_PARTY_NOTICES.md)。本记录不代表所有分发权利已经核实。

## 依据与授权边界

项目记录作者为 Tommy Wang。仓库中没有 CLA、版权转让或雇主授权文件。
导入材料和独立许可的贡献保留原条款。

原根目录 MIT 文件逐字节保存在 [LICENSES/MIT-legacy.txt](../../LICENSES/MIT-legacy.txt)，
此前 MIT 授权不撤回。第三方 MIT、Apache、GPL 等声明继续适用于原材料及此前按原许可
维护的修改。标准 AGPL 正文从 [GNU](https://www.gnu.org/licenses/agpl-3.0.txt) 原样下载，
SHA-256 为 `0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0`。

## 链接与分发边界

| 依赖 | 仓库依据 | 保持的边界 |
|---|---|---|
| HAROGIC SDK | `Makefile` 将 `backend/sdk/san90_sdk.cpp` 链接至外部 `libhtraapi.so.0.55.100`；Rust 加载项目适配库；`htra_api.py` 使用 ctypes。 | SDK 仍单独获取并安装在 `/opt/htraapi`，不加入 SDK 头文件／库；例外只提供项目方对组合的许可。 |
| FDK AAC | `dsp/core/build.rs` 原生端调用 `pkg-config --libs fdk-aac`，未用 `--static`；wasm32 使用静态 `libfdk.a`。 | 列入链接例外；Fraunhofer 完整 NOTICE 及源码、专利条件继续适用。 |
| libxaac | 同一构建脚本只在 wasm32 静态链接其归档库。 | 保留 Apache-2.0 及 LICENSE／NOTICE，不列入这两个依赖的例外。 |
| ggmorse／DeepFilterNet／其他 Rust 库 | C++ 编译归档及 Cargo 依赖 | 保留上游条款和声明，没有任意专有模块的通用豁免。 |
| ONNX Runtime／模型 | 原生降噪动态加载库／模型图 | 原库及模型许可和声明继续适用。 |
| RTTY 等 provider／Soapy 驱动 | 安装脚本及既有管道／网络接口 | 保持独立安装和原有 GPL／LGPL 边界。 |

本机 ELF／运行时检查确认服务依赖 `libfdk-aac.so.2`；`pkg-config` 报告 FDK **3.0.0**，
运行进程映射 `/usr/lib/libfdk-aac.so.2.0.4`。SDK 适配库映射单独安装的
`/opt/htraapi/lib/x86_64/libhtraapi.so.0.55.100`。这些信息标识本机二进制，
不能证明已提交 WASM 归档库的精确源码／补丁输入。

附加许可允许与 HAROGIC SDK 和／或 FDK AAC 静态／动态链接、运行时加载及分发组合程序。
自有代码、修改版本、适配器和构建安装脚本的源码义务，以及 AGPL 第 13 条的网络源码入口
均保留。它不免除第三方自身的源码要求，也不赋予被 SDK 协议禁止的厂商材料分发权。

## 源码入口与打包

`npm run build`（或 `npm run dev`）通过 Vite 调用 `tools/prepare_legal.py`，
准备本地许可／声明副本和带版本的源码归档。归档是生成产物，不提交 Git。公开 Git 源码入口可以
指向包含实际修改和构建输入的精确发行 commit/tag；移动分支或上游首页不代表修改版本的源码。在检出仓库中需要 Python 3.10+ 和 Git。
页脚链接至许可页及本次构建源码，源码链接标题包含版本标识；该过程不上传或对外发布。
开发服务器生成启动时的快照；修改源码后应重启开发服务器更新入口。生产构建每次重新生成快照。

版本标识按实际文件内容、路径和可执行位计算，包含未提交修改和新增项目源文件。
`source-manifest.json` 记录 Git 基准、文件哈希和归档摘要。归档包括仓库源码、锁文件、
fixture 和构建输入，排除 runtime 输出、生成的许可包、Git 内部文件，以及再分发授权
未确认的厂商 Python 包装。不收集 `/opt/htraapi` 的任何文件。解压后的归档可以凭其中的
`SOURCE-MANIFEST.json` 在没有 Git 的情况下重新构建。

打包前应从匹配的输入构建服务和前端。`tools/package-runtime.sh` 将前端许可／源码包
一起纳入 runtime，检查快照与当前项目输入一致，继续不复制 SDK 库。随服务保留可免费
下载的源码包。运营方若修改网络服务版本，应重新生成相匹配的源码入口；仅链接上游分支
或旧发行版并不是该修改版本的源码提供。

该快照提供项目树，不证明所有第三方二进制的完整对应源码已经到位。分发前仍须补齐所需
依赖源码、补丁、声明和安装信息。尤其是 FDK 上游当前分支链接或锁文件，不能解决已提交
codec 归档库缺少精确源码来源的问题。文档修改也不会自动重新授权或重建既有原生／JS／WASM
产物；历史 MIT 二进制仍保留原授权。

## 需要法律与来源核对的具体问题

| 项目 | 具体依据 | 待确认事项 |
|---|---|---|
| 版权／外部贡献 | 单一 Git 署名；原版权声明泛称 contributors；未发现转让／CLA。 | 核实作者、雇佣／合同权利，以及任何外来材料是否允许同时授予 AGPL-3.0-only 和链接例外；未确认的贡献保留或单列其原许可。 |
| HAROGIC SDK 与 Python 包装 | 已安装 `/opt/htraapi` 未发现 LICENSE、NOTICE 或 EULA；开发文档将 `htra_api.py` 标为厂商包装。SDK 还附带 liquid、MKL、USB、OpenMP 库。 | 取得实际适用协议，核对组合／链接／加载、包装／头文件复制修改、再分发限制及各附带库条款；公开可下载不等于已有这些授权。 |
| FDK AAC | [NOTICE](../../dsp/core/fdk/NOTICE) 第 2 节要求保留完整声明、向二进制接收者免费提供 codec 完整源码／修改、修改及日期声明和修改版本命名；第 3 节不授予专利，提及适当专利许可。 | 核实实际二进制的义务履行及适用专利授权。项目例外不授予专利，也不免除这些条件。 |
| Codec 二进制来源 | CI 固定 FDK 2.0.3 提交 `716f4394641d53f0d79c9ddac3fa93b03a49f278`；归档构建脚本接受任意源目录。本机 FDK 检出为 `22128505f539b90a25c7f7463abd00b5d544dee8` 且有 encoder 修改；libxaac 为 `2fbadd57bca46693b7a077bcf1513131106d960c` 且有 decoder 修改。 | 本机检出不能证明已提交归档库的输入。记录实际版本／补丁并提供对应源码，保留 FDK 修改／命名声明和 libxaac 的 Apache 第 4 条声明要求。本次不重建或替换 codec。 |
| DRM 来源 | `dsp/core/src/digital/drm/mod.rs` 称 GPL-2.0 的 Dream／DecDRM 只作算法参考、未复制代码；`dsp/util.rs` 又称移植 helper。此前 MIT-clean 表述不构成权利证明。 | 核查表达、表格和复制／改编来源；`dsp/util.rs` 暂不纳入新授权。若存在 GPL-2.0-only 内容，需核实兼容性或原作者另行许可；本例外无法豁免其权利。 |
| DFN3 模型 | 已提交 `DeepFilterNet3_onnx.tar.gz` 只有模型／配置，没有附许可；DeepFilterNet 源码有 MIT／Apache-2.0。 | 将该模型精确对应到上游发行、许可及哈希；不能仅由代码许可推断模型许可。 |
| CTY 国家数据 | `gen_cty.py` 指向 country-files.com／AD1C；`cty.dat` 未附许可。 | 取得允许再分发及生成数据使用的条款并保留必要署名；不对该数据主张 AGPL 换证。 |
| 字体 | 构建脚本对子集化的已安装 LXGW WenKai 进行提取；已复制本机 OFL 文本；上游[当前 OFL](https://github.com/lxgw/LxgwWenKai/blob/main/OFL.txt) 另有保留名称和 webfont 附加许可。 | 将已提交子集对应到精确字体发行版和适用的保留名称／子集化条款；不能用当前上游条款代替历史产物的条款。 |
| 其他依赖／provider | 保留上游文件、锁文件、安装脚本及模型声明。 | 核实实际分发所需的完整依赖声明和源码义务，包括内部附带原生库；进程独立本身不是通用的法律兼容性结论。 |

GNU AGPL [第 7、13 条](https://spdx.org/licenses/AGPL-3.0-only.html) 提供附加许可机制和
修改网络版本的源码要求。例外草案受授权人实际权利范围约束；上述事项仍需法律人员／权利人
核实，不能把新的对外分发视为已全部获准。

## 2026-11-08 追加核对证据

本地候选包核对补充了实际产物比对；公开下载不能代替适用协议，也不据此推断授权。

| 项目 | 新增证据 | 状态 |
|---|---|---|
| SDK | /opt/htraapi 仍未找到 LICENSE／NOTICE／EULA／协议，未取得实际适用的 SDK 协议。 | 仍须厂商协议；候选包不含 SDK 头文件和库。 |
| FDK | 在隔离目录以本机提交 22128505f539b90a25c7f7463abd00b5d544dee8、clang 23.1.3 和仓库脚本重建。本机差异 SHA-256 为 72dbe6dc9bf1e8c802b7867638b03e5c1769ebdd5e380bf1c4bb4c4fa31d1e9d。 | 重建哈希 8443a7eff7fd76e1ef6275110ae9e598929e1f26b1ed6c83e963ffe0e349a972 与已提交的 9115cb6dbabb895a966ffb6c13372973c1fde9cb1055f579bb0b821596d17232 不同。尚不能确认原始输入／工具链；未替换已提交归档。 |
| libxaac | 隔离重建本机提交 2fbadd57bca46693b7a077bcf1513131106d960c；decoder 补丁 SHA-256 为 a02c873c6e5db7d136bb307bbb9790232b33c4fddd2c0b08e999edb41d0bcfd4。 | 重建哈希 da732619175946a44bef290e2dd89ed6929027359fe68646e063c8bb646dca1f 与已提交的 49ca5e0bedacaced12e407c4f8f0788cac8d30b0bae9c30e65d10ab2c5c3d2d8 不同，原归档源码／补丁仍待确认。 |
| DFN3 | 已提交模型与[上游 v0.5.6](https://github.com/Rikorose/DeepFilterNet/blob/v0.5.6/models/DeepFilterNet3_onnx.tar.gz) 逐字节匹配，SHA-256 为 c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616。该发行版根 LICENSE 提供 MIT OR Apache-2.0。 | 已确认产物来源；模型归档内未单独记载模型专属授权。 |
| 字体 | 两个 WOFF2 的 name 表均标识 **1.522，2026-03-17**；各自 7,953 个映射字形的轮廓与已安装 1.522 Regular／Medium TTF 全部一致。现有许可与 [v1.522 OFL](https://github.com/lxgw/LxgwWenKai/blob/v1.522/OFL.txt) 完全匹配，SHA-256 为 c38b1994a5e48ac30ac7d1da7d0409fd8fd8127dfe28a13d6e787d5b1ef34a5e。 | 已确定历史版本／许可对应；该文本版权行后未声明保留字体名，不以当前分支条款代替此发行版。 |
| CTY | 本地数据未附再分发授权；上游 cty/readme.txt 查询返回 HTTP 404。 | 再分发及生成数据使用权仍待确认。 |

重建／补丁和字形比对的机器可读记录保存在本机
/tmp/websa-license-review-20261108/。这些核对缩小了未知范围；候选包仅作本地验收，
尚不表示对外发布许可已全部就绪。
