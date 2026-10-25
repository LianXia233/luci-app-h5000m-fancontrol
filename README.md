# H5000M Fan Control

[![CI](https://github.com/LianXia233/luci-app-h5000m-fancontrol/actions/workflows/ci.yml/badge.svg)](https://github.com/LianXia233/luci-app-h5000m-fancontrol/actions/workflows/ci.yml)
[![Build Release](https://github.com/LianXia233/luci-app-h5000m-fancontrol/actions/workflows/release.yml/badge.svg)](https://github.com/LianXia233/luci-app-h5000m-fancontrol/actions/workflows/release.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

面向 Hiveton H5000M 的 OpenWrt LuCI 风扇管理器。控制器是原生 Rust 程序，提供温度监控、
自动风扇曲线、手动 PWM、启停助推、温度滞回、降速延迟和传感器故障保护，并保留内核的
CPU 降频、高温及临界过热保护。

> H5000M fan manager for OpenWrt: a dependency-free Rust controller with
> temperature-aware profiles, manual PWM, hysteresis, delayed spin-down, start
> boost and sensor failsafe — the kernel's thermal protection stays intact.

![散热管理界面（3.2.0 实机截图）](docs/fan-control-ui.png)

*上图为 3.2.0 在 H5000M 实机上的概览页截图：拟物涡轮风扇图、270° 进度环、
输出仪表与寄存器回读、状态卡（含控制温度与控制归因）、全传感器温度带
（含来自 mt5700 管理器桥接的 5G 模组温度）。底部的"保存并应用 / 保存 / 重置"
是 LuCI 原生的未保存更改指示器，插件不再自带保存按钮。*

版本采用 `主版本.次版本.修订版本-r打包修订`。GitHub Release 使用语义版本标签
（当前 `v3.2.0`），OpenWrt 安装包版本为 `3.2.0-r1`。

## 3.2.0 变更要点

- **5G 模组温度接入**。守护进程通过本机 `luci-app-mt5700` 管理器的 RPC
  节流查询 `AT^CHIPTEMP?`（默认 30 秒一查，60 秒新鲜度），取 12 路结温中
  有效读数的最高值写入 `/var/run/mt5700m/temperature`；串口仍由管理器独占，
  管理器缺席时模组传感器照常报告缺失。
- **保存走 LuCI 原生流程**。页面移除自带的保存/保存并应用/恢复按钮：修改先
  暂存 UCI，由原生"未保存的更改"指示器负责提交、回滚保护与服务通知；
  控制器监听配置文件变化自动生效，"立即应用"按钮保留用于即时生效。
- **界面重绘**。风扇图与曲线图换为拟物渐变风格的原生 SVG（八叶涡轮、270°
  进度环、随输出变速的转子、曲线下方面积、上下限行内标注、光标束与悬浮
  读数胶囊），动画尊重 `prefers-reduced-motion`。
- **概览排版修正**。补齐缺失的 `span3` 栅格定义，状态卡收纳控制温度与
  控制归因，卡片末行统计撑满剩余高度；移除本机型给不出的转速反馈卡片。

## 3.0.0 变更要点

- **控制逻辑迁到 Rust**。`/usr/sbin/h5000m-fancontrol` 现在是交叉编译的静态
  aarch64 二进制（零 crates.io 依赖），逐周期采样、判定、写 PWM、发布快照与事件日志。
  Shell 脚本只保留安装、启动与升级职责，状态命令退化为一个 `exec` 包装。
- **结构化状态接口**。`h5000m-fancontrol status json` 输出固定字段契约，前端不再
  切分 `key=value` 文本；`schema` 子命令由写入器自身导出字段清单，CI 双向比对
  前后端字段。遗留 `key=value` 仍保留全部 2.x 键。
- **温度缺失一律是未知**。不可读、超界、陈旧的传感器在 JSON 里是 `null`、在
  `key=value` 里是空串，绝不折算成 `0 °C`，也不会进入控制输入。
- **界面重做**。六个 TAB（总览 / 风扇控制 / 曲线 / 温度监控 / 系统设置 / 诊断），
  原生 SVG 风扇图、曲线图、温度仪表，深色/浅色与 `prefers-reduced-motion`，
  简体中文文案走 gettext 目录；TAB 选择持久化，重载后回到原 TAB。
- **不再展示硬件给不出的数值**。设备无 tach 反馈时不显示 RPM/风量估算，只并列展示
  设定值与接口实测回读。
- **并发与生命周期**。procd 前台运行、SIGHUP 重载、目录锁互斥；已有守护进程存活时
  `apply` 只写唤醒标记并排队，不与之争用 PWM 节点。
- **打包与发布**。APK/IPK 由官方 OpenWrt SNAPSHOT SDK 构建，发布前显式签名并严格
  验签，公开校验值与公钥；私钥不进仓库、不进包、不作为构建产物发布。
- 完整的功能映射与等价性论证见 [docs/refactor-mapping.md](docs/refactor-mapping.md)。

## 功能

- 汇总 CPU、以太网 PHY、Wi-Fi 射频及 5G 模组温度，原生读数优先，模组温度经
  mt5700 管理器 RPC 桥接自缓存，MTK 厂商驱动 `iwpriv` 有界超时回退
- 静音、均衡、性能和自定义四种自动曲线（自定义曲线保存前校验并线性插值）
- 自动、手动和仅内核保护三种运行模式
- 温度滞回及降速延迟，减少风扇频繁波动
- 风扇停转后的启动助推
- 传感器或曲线异常时自动进入高输出安全模式
- 读取设备树 `pwm-fan` 冷却档位与 cooling-maps，计算内核安全下限，同时展示
  实际输出与目标输出
- 配置修改走 LuCI 原生未保存更改流程（暂存 → 原生应用/回滚）
- 简体中文 LuCI 界面
- 不依赖云服务，不收集或上传设备数据

## 兼容性

- Hiveton H5000M（OpenWrt SNAPSHOT `mediatek/filogic`，MT7987A / ARMv8 64-bit，musl）
- OpenWrt SNAPSHOT（LuCI JavaScript 视图）
- `pwm-fan` 驱动及 `/sys/class/hwmon/*/pwm1` 控制节点

本项目针对 H5000M 的设备树和传感器布局设计。其他设备即使能够安装，也不建议直接使用。

## 安装

### 使用预编译包

从 GitHub Releases 下载与设备 ABI 一致的近期 SNAPSHOT `mediatek/filogic` APK
（应用包与 `luci-i18n-h5000m-fancontrol-zh-cn` 语言包），连同 `SHA256SUMS`、
`BUILD-INFO.txt` 与本次构建的公钥一起放到设备上后校验并安装。每次发布由 SDK
现场生成签名密钥，安装前先把发布页的 `openwrt-sdk-build.pem` 放进
`/etc/apk/keys/`（文件名随意，如 `h5000m-build.pem`）：

```sh
sha256sum -c SHA256SUMS --ignore-missing
cp openwrt-sdk-build.pem /etc/apk/keys/h5000m-build.pem
apk add luci-app-h5000m-fancontrol-3.2.0-r1.apk luci-i18n-h5000m-fancontrol-zh-cn-*.apk
```

不导入公钥时也可以用 `apk add --allow-untrusted` 安装（本地校验由
`SHA256SUMS` 承担）。发布页同时提供 SDK 校验值、源码提交与公钥，包由 SDK
自带的 `apk adbsign` 签名并用 `apk verify` 严格验签后才上传；私钥不进入仓库
与发布产物。构建过程与密钥处理见 [docs/build.md](docs/build.md)。

### 集成到 OpenWrt 源码

在 OpenWrt 源码根目录执行：

```sh
git clone https://github.com/LianXia233/luci-app-h5000m-fancontrol.git \
  package/luci-app-h5000m-fancontrol

make menuconfig
# LuCI -> Applications -> luci-app-h5000m-fancontrol

make package/luci-app-h5000m-fancontrol/compile V=s
```

源码构建需要宿主 Rust 工具链与 `aarch64-unknown-linux-musl` target；
`src/Makefile` 会校验产物 ELF 架构，防止把宿主机二进制打进包。

由于风扇安全策略依赖设备树，建议把本项目集成进固件并同时评估下方补丁，
而不是只安装软件包。

### 从 2.x 升级

安装新包后 `/etc/init.d/h5000m-fancontrol restart` 即可。`uci-defaults` 会把
2.x 配置幂等地迁移到 `schema_version 3`：保留原有曲线与阈值，把非法值回收为
出厂默认并记录原因，配置内容未变化时不重写、不重启服务。迁移行为由
`tests/test-uci-defaults.sh` 覆盖。

## 独占风扇策略控制

H5000M 原设备树中的主动散热映射会与用户空间控制器同时修改 PWM。若希望自动曲线完整
控制风扇，请在编译固件前应用项目提供的补丁：

```sh
git apply package/luci-app-h5000m-fancontrol/openwrt-patches/h5000m-userspace-fan-control.patch
```

该补丁仅删除 H5000M 的三个风扇 cooling-map；CPU 降频、hot 和 critical 温控节点仍然
保留。控制器还会在 CPU 达到高温阈值时强制提高风扇输出。

不应用补丁时插件仍可运行，但内核 thermal governor 可能提高实际 PWM，因此界面中的
请求输出和实际输出可能不同。

## 配置与服务

- UCI 配置：`/etc/config/h5000m_fancontrol`（选项、范围与默认值见 [docs/uci.md](docs/uci.md)）
- procd 服务：`/etc/init.d/h5000m-fancontrol`（`restart` / `reload` 走 SIGHUP）
- 控制器：`/usr/sbin/h5000m-fancontrol`
- 状态命令：`/usr/sbin/h5000m-fancontrol-status`（包装到 `h5000m-fancontrol status`）
- LuCI 页面：系统 → 风扇控制

常用命令：

```sh
h5000m-fancontrol status          # 遗留 key=value 输出
h5000m-fancontrol status json     # 结构化状态（默认读取快照，不重采样）
h5000m-fancontrol status live     # 强制全量重采样
h5000m-fancontrol apply           # 立即执行一个控制周期
h5000m-fancontrol validate        # 只校验配置，不写硬件
h5000m-fancontrol events 50       # 事件日志
h5000m-fancontrol schema          # 导出状态字段契约
h5000m-fancontrol release         # 恢复遗留 thermal policy
```

## 安全说明

**禁止削弱或绕过 Linux 内核的 CPU 降频、高温及 critical 过热保护。** 控制器不会把
整个 CPU thermal zone 的策略切换为 `user_space`，也不会为了接管风扇而删除 CPU 热
保护节点；若发现历史版本遗留的 `user_space` 接管，它会在启动时一次性恢复内核策略。
无论是否应用独占风扇补丁，都不应删除设备树中的 CPU 降频、hot 或 critical 保护。

风扇控制属于设备安全功能。修改自定义曲线或手动 PWM 后应持续观察温度；页面会显示
内核保护是否完整、当前安全下限来自哪个触发点，以及输出是否被下限抬高。
不变式与决策顺序见 [docs/safety.md](docs/safety.md)。

## 验证状态

离线验证全部通过：`cargo fmt`/`clippy -D warnings`、115 个单元测试 + 39 个回归测试、
前端渲染冒烟（298 项）、状态契约与遗留输出测试、UCI 迁移测试、打包守卫、内核热保护
红线守卫（含反向验证）、shellcheck、语言包完整性审计、aarch64 musl 静态交叉编译。
细节见 [docs/testing.md](docs/testing.md)。

以下内容已在 H5000M 实机验证（2026-10-25/26，ImmortalWRT SNAPSHOT）：

- 从 2.1.0 与 3.0.0 原地升级到 3.2.0：配置幂等迁移（schema v2/v3 → 用户设置保留）、
  服务无停机接管；
- 六标签页在设备 LuCI（Aurora 主题）下的实机渲染与交互、三卡片概览布局、
  涡轮风扇图与曲线图动画；
- 5G 模组温度桥接：30 秒节流自缓存写入 `/var/run/mt5700m/temperature`，
  温度页实时显示模组读数；
- 自动模式控制循环：多传感器取最大值、目标/请求/回读三级输出一致、
  降速延迟与内核下限并列展示；
- LuCI 原生未保存更改流程下的配置暂存与生效。

仍建议长期观察（尚未覆盖）：连续多日运行稳定性、真实传感器故障下的失效保护路径、
厂商 `iwpriv` 回退在真实 mt_wifi 驱动上的输出格式、长时间运行下的转速读数可用性
（本机型固件未提供 `fanN_input`）。完整清单与推荐验证顺序在
[docs/testing.md](docs/testing.md) 末节。

## 文档

- [架构](docs/architecture.md) — 模块划分、数据流、并发与生命周期
- [功能映射](docs/refactor-mapping.md) — 2.1.1 Shell → 3.0.0 Rust 的等价性对照
- [状态接口](docs/status-api.md) — JSON 字段表与缺失值语义
- [热安全](docs/safety.md) — 不变式、下限计算、决策顺序
- [UCI 配置](docs/uci.md) — 选项、范围、迁移
- [构建与发布](docs/build.md) — 交叉编译、SDK、签名
- [测试](docs/testing.md) — 覆盖内容与未验证项
- [故障排查](docs/troubleshooting.md)

## 许可证

[Apache License 2.0](LICENSE)
