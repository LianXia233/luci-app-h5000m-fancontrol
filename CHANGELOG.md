# Changelog

## 3.0.0

- 控制器由 shell 脚本迁移为原生 Rust 守护进程：无第三方依赖、全静态 musl 链接，经 OpenWrt SDK 交叉编译；procd 前台运行，SIGTERM 清理锁与遗留 policy 文件，SIGHUP 立即重载。
- UCI 配置读取改为单次 `uci show` 并按配置文件 mtime 缓存，替代每周期约 18 次 `uci get`。
- 厂商 `iwpriv` 温度回退由 Rust 原生限时（1 秒/查询），不再依赖外部 `timeout` 命令。
- 状态模型统一为 JSON（`status --json`，前端快照 + 过期回退实时查询）与传统 key=value 双输出；守护进程每控制周期刷新快照。
- LuCI 界面重建为多标签页并新增 SVG 风扇/曲线可视化，简体中文翻译随界面重新生成。
- 回归测试迁移为 Rust 单元测试（60 项）与模拟 sysfs/设备树的 shell 场景测试；CI 覆盖 fmt、clippy、单元测试、aarch64 musl 交叉编译、前端字段契约、翻译覆盖、shellcheck 与打包检查。
- 修复测试夹具中 `write_be32` 使用 awk 字段引用 `$i`（BEGIN 中为空）导致设备树属性被写成全零的问题，改用 `ARGV[i]`；并加 `LC_ALL=C` 防止 UTF-8 locale 下 awk `%c` 把 ≥128 的字节编成多字节 UTF-8（如 160 写成 `c2 a0`）而损坏夹具——该问题曾让内核下限测试在 CI 上必然失败。
- 修复 `src/src/proc.rs` 测试代码使用不稳定特性 `str::as_str()`（`str_as_str`）导致稳定版 clippy 报 E0658 的问题。
- 修复 CI secrets 检查匹配到工作流自身的 grep 模式导致必然失败的问题（排除 `.github` 目录）。
- 修复 CI 前端检查在 ubuntu-latest 上因缺少 gettext（`msgfmt: command not found`，退出码 127）而失败的问题：安装 gettext 后再做翻译校验。
- 修复 SDK 打包失败：luci.mk 以普通子 make 调用 `src/Makefile install` 时不提供 OpenWrt 的 `INSTALL_DIR`/`INSTALL_BIN`（空展开导致 error 127），改为缺失时回退到规范定义；同时修正 `BINARY` 路径缺少 `target/` 前缀的问题——原路径下 `file` 检查会因错误消息里含目标架构字样而被静默绕过。
- 修复 shellcheck 警告（init.d 的 rc.common 变量、SHA256SUMS 同文件读写）导致的打包检查失败。
- 补全 .po 头部字段并更新 check-po.js 以支持多行 msgstr。

## 2.1.1

- 支持 MTK 厂商 Wi-Fi 驱动的温度回退读取（#2）：优先使用原有 thermal/hwmon；只有对应读数缺失、接口存在且可用时，才通过 `iwpriv ra0/rai0 stat` 读取 `CurrentTemperature`。
- 每个回退查询最多 1 秒，只接受 0–125°C 的整数；命令失败、超时或格式错误保留“无读数”，不当成 0°C。不安装或调用任何无线配置命令。
- `iwpriv` 和 `timeout` 为可选能力，缺失时跳过回退；`max` 模式仍取所有有效传感器最高温度，原内核温控保护和安全下限不变。
- 增加厂商温度、异常值/超时、原生传感器优先与温控保护回归测试；发布构建先通过 CI。真实厂商驱动环境仍需安装者复测反馈，不能将模拟测试当作全平台实机认证。
- APK 发布前显式签名并严格验签，同时提供 SDK 校验值、源码提交和包 SHA256；仅发布构建公钥，不发布私钥。

## 2.1.0

- 风扇控制器不再把完整 CPU thermal zone 切换到 `user_space`。
- 自动恢复旧版本遗留的 thermal policy，并保留 CPU 降频和高温保护。
- 状态读取与控制写入使用独立 RPC 权限。
- 将 UCI 配置声明为升级保留文件。

## 2.0.2

- 读取 MT5700M 管理器提供的带时间戳温度缓存。
- 忽略超过 60 秒的陈旧模组温度，并兼容旧 QModem 缓存。

## 2.0.1

- 统一 GitHub Release 与 OpenWrt 软件包版本规则。
- 汇总 2.0.0 后的构建兼容性与交付修订。

## 2.0.0

- 重新设计面向最终用户的 LuCI 散热管理界面
- 增加 CPU、以太网 PHY、Wi-Fi 和 5G 模块温度汇总
- 增加静音、均衡、性能及自定义风扇曲线
- 增加温度滞回、降速延迟和启动助推
- 增加传感器故障保护及高温用户空间安全下限
- 增加可选 H5000M 设备树补丁，避免内核与用户空间争用 PWM
