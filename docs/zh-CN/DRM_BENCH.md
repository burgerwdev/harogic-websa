# DRM 台架环回（PlutoSDR → SAN-90）

使用可重复生成的本地信号采集 DRM 基带，并对比解码器结果。夹具来源、清单、格式与重新生成方法见 [DRM 夹具 README](../../tests/fixtures/drm/README.md)。本指南不包含新的硬件验证结果。

## 配置

[电台配置](../../tools/bench/drm_bench/station_iq.toml) 当前生成 mode B、SO3（10 kHz）、64-QAM MSC、16-QAM SDC、长交织、保护 A/B 0/1，以及标为 `SAN90 DRM BENCH` 的单声道 **xHE-AAC** 1 kHz 单音。IQ 输出为 int16 立体声（左声道为 I），电平 -15 dBFS；单音电平 -12 dBFS。已提交的真实抓取早于这份配置，承载的是 HE-AAC（12 kHz 核心 + SBR，24 kHz PCM），不是 xHE-AAC。

已有记录使用 PlutoSDR 发射、SAN-90 外接天线接收，收发相距 2 米：

- 发射本振：400 MHz；DRM 偏移：+100 kHz，以避开本振泄漏。
- 分析仪：SDR 模式，中心与收听频率 400.1 MHz，decimate 256（约 195 kHz IQ 采集带宽），参考电平 -40 dBm，DRM 解调。
- 使用 15 秒 IQ 文件。60 秒文件需要约 250 MB 循环缓冲，在记录中的 Pluto 配置上失败；15 秒需要约 63 MB。

## 生成、发射与采集

从本仓库运行，外部 DecDRM 的 `decdrm` 可执行文件须在 `PATH` 中：

```sh
decdrm tx tools/bench/drm_bench/station_iq.toml --output drm_iq_15s.wav --duration 15
python3 tools/pluto/pluto_drm_tx.py --iq drm_iq_15s.wav --lo 400e6 --gain -20 --seconds 600
```

抓取前启动服务并配置 SDR 会话。在频谱中确认 400.1 MHz 处有约 10 kHz 宽的信号。抓取需要 Python `aiohttp` 和 NumPy：

```sh
python3 tools/bench/drm_capture.py --seconds 10 --out /tmp/drm_live.f32 --json /tmp/drm_live.json
```

工具订阅 `/ws?iq=1&noaudio=1`，不会配置或打开设备。它保存 IQBF wire v2 帧中的小端 float32 交织 I/Q，以及采样率、电平和传输元数据；测量前先丢弃一秒的排队帧。离线解码应使用上报的采样率；已提交真实夹具上报的采样率为 48828.125 Hz，而非 DRM30 内核的 48 kHz。采集电平与历史射频测量结果不能作为新会话的校准常数。

## 离线对比

已提交抓取与合成夹具见[夹具 README](../../tests/fixtures/drm/README.md)。原生接收机夹具命令为：

```sh
cargo test --manifest-path wasm/Cargo.toml --release --test drm_live_fixture -- --nocapture
```

此命令读取已提交夹具，不读取 `/tmp/drm_live.f32`。原生编解码器构建需要 `pkg-config` 与支持 DRM/USAC 的 FDK-AAC >= 2.0.3；参见 [AAC 格式与构建说明](DRM_AAC_RESERIALIZATION.md)。

要独立对比新采集的数据，请提供支持 `--status-socket` 的外部 Dream console 二进制，并使用采集时上报的实际采样率：

```sh
python3 tools/analysis/drm_oracle_check.py /tmp/drm_live.f32 48828.125 --dream-bin /path/to/dream
```

对照工具重采样到 48 kHz 并归一化，再写出 Dream 所需的 int16 IQ WAV。Dream 与 DecDRM 均为外部依赖；此处不提供其二进制或外部构建说明。

## 音频说明

DRM 音频必须按信令中的编解码配置解帧。text 标志置位时，每个音频逻辑帧最后四字节是文本，必须在拆分接入单元前移除，否则会错移从末尾反向读取的 SBR 负载。历史 HE-AAC 抓取每个 400 ms 帧含五个 208 字节接入单元，以 24 kHz PCM 输出 1 kHz 单音。

直接 AAC-LC 夹具使用不带 SBR 的 24 kHz 核心，输出 24 kHz。合成端到端 AAC IQ 夹具在同一核心上声明 SBR，因此报告 48 kHz 输出。应使用解码器返回的采样率与声道数，而不是由夹具文件名推断速率。流式解码还依赖跨输入块持续保存 MSC 解交织与载波校正状态。
