# 重构前后功能映射表（2.1.1 Shell → 3.0.0 Rust）

基线提交：`5a9e71c`（`fix: support bounded MTK vendor radio temperatures; release 2.1.1`）。
原实现为单文件 `root/usr/sbin/h5000m-fancontrol`（631 行 POSIX shell），
新实现为 `backend/` Rust crate（零外部依赖），安装后仍占用同一路径。

本表用于评审逐条核对：每一行都必须能在 Rust 侧找到对应实现与测试。

## 1. 调用链

原实现：

```
procd (START=98) ──> /etc/init.d/h5000m-fancontrol start_service
                     └─ /usr/sbin/h5000m-fancontrol daemon   # 前台循环
                          └─ apply_once ─┬─ load_config      # uci -q get（多次进程启动）
                                         ├─ find_pwm
                                         ├─ read_sensors     # cat/sed/awk/date/iwpriv+timeout
                                         ├─ set_thermal_floor# dd/hexdump/readlink
                                         ├─ calc_curve_pwm   # $(( )) 整数运算
                                         └─ echo > pwm1      # PWM 写入

LuCI 视图 ──fs.exec──> /usr/sbin/h5000m-fancontrol-status ──> h5000m-fancontrol status  # 全量重采样
LuCI 保存 ──────────> /etc/init.d/h5000m-fancontrol restart                             # 每次保存都重启
uci-defaults 91-… ──> schema<2 时补默认值 + enable + restart
```

新实现：

```
procd ──> /etc/init.d/h5000m-fancontrol ──> h5000m-fancontrol daemon（Rust，前台，无子进程）
   daemon::run ──> config::load（直接解析 /etc/config 文件，零进程启动）
                ──> controller::run_cycle
                       ├─ sensor::sample_all       （thermal/hwmon/厂商回退/模组缓存）
                       ├─ safety::evaluate         （DT cooling-map 安全下限 + 内核保护核对）
                       ├─ controller::decide       （纯函数：曲线/滞回/降速延迟/故障保护）
                       └─ controller::actuate      （启动助推 + PWM 写入 + 回读校验）
                ──> state::write（原子 tmp+rename）
LuCI 视图 ──> h5000m-fancontrol-status json  # 读取状态快照，不重复采样硬件
LuCI 保存 ──> h5000m-fancontrol apply        # 持锁则唤醒守护进程，不重启服务
```

## 2. 配置项（`/etc/config/h5000m_fancontrol`，段 `settings`）

| UCI 选项 | 原校验（`cfg_uint`/`cfg_string`） | 默认值 | 新模块 | 新行为 |
|---|---|---|---|---|
| `enabled` | 非负整数，夹取 0..1 | 1 | `config` | 同范围；非数字→默认并记入配置诊断 |
| `mode` | `auto\|manual\|kernel`，遗留 `off`→`kernel`，其他→`auto` | `auto` | `config` | 同上，并记录迁移事件 |
| `curve` | `silent\|balanced\|performance\|custom`，其他→`balanced` | `balanced` | `config` | 同上 |
| `temp_source` | `max\|cpu`，其他→`max` | `max` | `config` | 同上 |
| `manual_pwm` | 0..255 | 160 | `config` | 同上 |
| `interval` | 2..60 | 5 | `config`/`daemon` | 同上 |
| `hysteresis` | 0..10 | 2 | `config` | 同上 |
| `down_delay` | 0..300 | 30 | `config` | 同上 |
| `start_pwm` | 80..255 | 192 | `config` | 同上 |
| `start_boost_ms` | 0..3000 | 700 | `config` | 同上；改为精确毫秒睡眠 |
| `fail_pwm` | 192..255 | 255 | `config` | 同上 |
| `curve_silent` | 字符串 | `20:0,35:0,40:50,55:50,70:60,85:75,95:100` | `config`/`curve` | 曲线文本严格校验（见 §4） |
| `curve_balanced` | 字符串 | `20:0,35:0,40:50,55:60,70:75,85:90,95:100` | 同上 | 同上 |
| `curve_performance` | 字符串 | `20:0,30:60,45:70,60:82,75:92,85:100` | 同上 | 同上 |
| `curve_custom` | 字符串 | `20:0,40:50,55:60,70:75,85:90,95:100` | 同上 | 同上 |
| `schema_version` | uci-defaults 写入 | 2 | `uci-defaults` | 升级到 3，幂等 |

`/etc/uci-defaults/91-h5000m-fancontrol-v2` 的 v2 迁移（删除 `min_pwm`/`max_pwm`/`low_temp`/`high_temp`、
`off`→`kernel`）必须继续对老用户生效；新增 `92-h5000m-fancontrol-v3` 只做 v3 增量，不重复 v2 逻辑。

## 3. 传感器

| 能力 | 原实现 | 新模块 | 语义保持点 |
|---|---|---|---|
| CPU 温度 | `thermal_zone*` 的 `temp`，`type` 小写含 `cpu\|soc\|mt798\|thermal`；再 `hwmon*` 同规则；首个命中生效 | `sensor` | 优先级顺序、首个命中生效 |
| 以太网 PHY | `hwmon` `name` 含 `mdio\|phy` | `sensor` | 同上 |
| Wi-Fi 射频 1/2 | `hwmon` `name` 含 `mt799\|wifi\|radio`，前两个分别入 wifi1/wifi2 | `sensor` | 同上 |
| 厂商射频回退 | 仅当对应槽位缺失：`timeout 1 iwpriv ra0/rai0 stat`，取 `CurrentTemperature`，只接受 ≤125 的纯数字 | `sensor` | 有界超时；`iwpriv` 缺失即跳过；不做无界 ioctl |
| MT5700M 模组 | `/var/run/mt5700m/temperature` 的 `temperature=`（一位小数四舍五入，远离零）与 `updated=`，陈旧 >60s 丢弃；回退扫 `/tmp/cache_base_info_*`、`/tmp/cache_*info*` 的 `"key":"temperature"…"value":"NN"` | `sensor` | 陈旧→未知，绝非 0 °C |
| 温度取值 | 纯整数（可带负号）；绝对值 >1000 视为 mK 除 1000；结果必须在 −40..150 | `sensor::read_temp_c` | 同一算法（含截断方向） |
| 控制温度 | `cpu` 只看 CPU；`max` 依次比较 CPU、PHY、wifi1、wifi2、模组，严格 `>` 才替换（并列保首个） | `sensor::select_control_temp` | 并列取先者 |
| 缺失表示 | shell 变量为空字符串 | `SensorReading::status` + JSON `null` | 未知 ≠ 0 |

## 4. 曲线与输出计算

`calc_curve_pwm` → `curve::pwm_for`：

- 每点必须 `temp`/`percent` 均为非负整数，`temp ≤ 150`，`percent ≤ 100`，`temp` 严格递增；否则整条曲线无效。
- 低于首点→首点百分比；高于末点→末点百分比；中间线性插值 `prev_p + (t-prev_t)*(p-prev_p)/(t-prev_t)`，整数除法向零截断（与 shell `$(( ))` 一致）。
- 输出 `percent * 255 / 100`（向零截断）。
- 新增：解析错误携带可定位的中文错误码（前端展示），并在 `status` 中暴露 `curve_valid`/`curve_error`。

## 5. 内核热保护与风扇安全下限

| 原实现 | 新模块 | 说明 |
|---|---|---|
| `find_thermal_zone`：要求 `policy` 与 `available_policies` 可读且含 `user_space`；优先 `type` 含 `cpu/soc/mt798` | `safety::find_cpu_zone` | 仅用于**读取**内核状态；放宽为“不要求 `user_space`”，因为新版不再切换 governor |
| `take_thermal_control`：**不写** policy；只在存在遗留 `policy` 文件时调用 `release_thermal_control` | `safety::restore_legacy_policy` | 绝不把 CPU thermal zone 切到 `user_space`；保留一次性恢复 |
| `release_thermal_control`：按记录写回原 policy，未知时用 `step_wise`，删除文件 | `safety::restore_legacy_policy` | 逐条保持 |
| `cooling_device_path`/`max_state` | `fan::cooling_device` | 同上 |
| `read_be32`（`dd`+`hexdump`） | `safety::dt_be32` | 大端 u32，按 4 字节索引 |
| `cooling_level_for_state`（`of_node/cooling-levels`，>255 视为无效） | `safety::cooling_level` | 同上 |
| DT cooling-maps 扫描：`cooling-device` 首 phandle 匹配风扇 phandle→取第二 u32 为 state，`trip` phandle 匹配 trips→`temperature-hysteresis` 阈值；`CPU_TEMP ≥ 阈值-迟滞` 时取最大 state | `safety::floor_from_cooling_maps` | 完整移植，含“直接写 PWM 也会改 `cur_state`，所以 `cur_state` 不能当安全下限”的理由 |
| `THERMAL_OWNER`：`shared`/`userspace`/`kernel`/`unknown`（`apply` 时 `unknown`→`userspace-pwm`） | `safety::Ownership` | 同一取值集合，前端按值文案化 |
| 独占模式紧急下限：CPU 未知→state 3、floor 255；≥83 °C→state 2；≥113 °C→state 3 | `safety` | 逐条保持 |
| 共享模式下 CPU 未知→floor 255 | `safety` | 保持 |
| DT 探测失败的保守回退阈值 38/83/113 | `safety` | 保持 |
| 目标值取 `max(请求值, floor)`，并在 reason 追加 `+kernel-floor` | `controller` | 保持；额外导出布尔 `kernel_floor_applied`，不再靠字符串拼接表达 |
| 新增 | `safety::kernel_protection` | 上报 policy、passive/hot/critical 触发点、cpufreq cooling device 是否存在，供前端与回归测试断言“内核保护未被削弱” |

`openwrt-patches/h5000m-userspace-fan-control.patch` 的目的与适用条件不变（只删 3 个风扇 cooling-map，
保留 CPU 降频/hot/critical），文档继续说明“不打补丁时内核 governor 可能抬高实际 PWM”。

## 6. 控制循环决策（`apply_once` → `controller::decide` + `controller::actuate`）

保持原顺序与原判据：

1. 未使能或 `mode=kernel`：不写 PWM，读取当前值作为 `target/applied`，`reason=kernel`，`thermal_owner=kernel`。
2. `manual`：`requested=manual_pwm`，`reason=manual`。
3. 自动且控制温度缺失：`requested=fail_pwm`，`reason=sensor-failsafe`，`result=failsafe`。
4. 自动且曲线无效：`requested=fail_pwm`，`reason=curve-failsafe`，`result=failsafe`。
5. 自动正常：`requested=curve(control_temp)`，`reason=auto`。
6. `requested` 夹取 0..255；`target=max(requested, floor)`。
7. 降速延迟（仅 `auto` 且 `result=ok` 且 `target < last_applied`）：
   用 `control_temp + hysteresis` 重算候选并再次抬到 floor 之上；
   `down_since` 取状态值或当前时间；`now - down_since < down_delay` 时**维持** `last_applied`，
   `reason=auto-down-delay`；到期则清零 `down_since`。
8. 启动助推：`current == 0 && target > 0 && start_boost_ms > 0` → 先写 `max(start_pwm, target)`，
   睡眠（原版向上取整到整秒，新版精确毫秒），再写目标值。
9. `current != target` 才写入；写失败记日志并按失败退出该周期。
10. 回读 `applied_pwm`（读失败退回目标值），仅在 `result` 跃迁时写日志，避免刷屏。

修复的原缺陷（均为“更安全”方向，不改变正常路径行为）：

| 缺陷 | 影响 | 处理 |
|---|---|---|
| `apply` 与 `daemon` 各自独立抢锁，锁在周期之间释放 | 两个写入者可在同一周期交错，PWM 竞写 | 单一锁目录（沿用 `/var/lock/h5000m-fancontrol.lock`）+ `apply` 检测到存活守护进程时改为“唤醒”，不再并发执行 |
| `take_thermal_control` 读不到 policy 时整个周期返回 1 | 风扇不被更新（可能留在低速） | 仅记为“内核状态未知”，继续温控（更安全） |
| 找不到 PWM 节点时不写状态文件 | 前端只显示“状态过期”，无原因 | 写入 `result=error`、`reason=pwm-missing` 快照并进入事件日志 |
| `last_pwm` 无条件取旧状态 | 语义上无风险但重启后可能长时间维持高转速 | 状态快照陈旧（>300 s）时改用实测当前 PWM |
| `reason` 被 `auto-down-delay` 覆盖，丢失 floor 信息 | 前端归因不准 | 保留字符串兼容，同时导出 `kernel_floor_applied` |
| 每次循环 spawn `date`/`uci`/`cat`/`sed`/`dd`/`hexdump` | CPU/进程开销大，工具缺失即静默失效 | 全部改为 std 文件 IO；仅厂商回退保留一个有界子进程 |

## 7. 状态与接口

`state` 文件（`/var/run/h5000m-fancontrol.state`）仍是原子写入的 `key=value` 快照，
新增字段为**追加式**（旧消费者不受影响）：每个传感器的 `value/status/label/source/sampled_at/error`、
`cycle_count`、`consecutive_failures`、`last_error`、`pid`、`controller_version`、`boost_applied`。

CLI（严格保持既有语义）：

| 命令 | 原行为 | 新行为 |
|---|---|---|
| `apply`（默认） | 单周期执行 | 同；若锁被存活守护进程持有则唤醒并返回 0 |
| `status` | 全量重采样并输出 `key=value` | `key=value` 保持全部原字段；默认改为“快照 + 廉价实时读（PWM/RPM）”，`status live` 保留全量重采样 |
| `daemon` | procd 前台循环 | 同；信号驱动优雅退出、唤醒标记、配置变更即时生效 |
| `release` | 恢复遗留 thermal policy | 同 |
| 新增 | — | `status json`、`events`、`validate`、`--version`、`--help` |

环境变量测试接口保留原名：`H5000M_FAN_SENSOR_SYSFS_ROOT`、`H5000M_FAN_RADIO_SYSFS_ROOT`、
`H5000M_FAN_MT5700M_TEMP_CACHE`、`H5000M_FAN_MT5700M_TEMP_MAX_AGE`；
新增 `H5000M_FAN_DT_ROOT`、`H5000M_FAN_UCI_CONFIG`、`H5000M_FAN_RUN_DIR`、`H5000M_FAN_LOCK_DIR`、
`H5000M_FAN_PROC_ROOT`。所有硬件路径均可指向隔离目录，测试永不触碰真实 PWM 节点。

## 8. 前端字段一致性

`docs/status-api.md` 固定 JSON 字段表。字段清单不再手工维护：`h5000m-fancontrol schema`
由写入器自身导出（`json::Value::paths()` 递归 `report::contract()` 生成的报告，数组折叠为
`父[].子`），因此清单不可能与实现漂移。`tests/test-status-contract.sh` 在 CI 中双向比对：
清单必须覆盖前端消费的每一个字段，且每个字段必须真的出现在 `fancontrol.js` 里，缺一边即失败。
同一脚本还断言遗留 `key=value` 输出保留全部 2.x 键、未读传感器打印为空而非 `0`，
并用隔离目录跑一次完整控制周期（写入 PWM、发布快照与事件、锁竞争时排队不覆写、
风扇节点缺失时报 `pwm-missing` 且 `apply` 非零退出）。用于满足“前后端数据字段一致性”回归要求。

## 9. 测试分层

2.x 的三个 shell 测试（`test-module-temperature.sh`、`test-thermal-policy-safety.sh`、
`test-vendor-temperature.sh`）已删除，其断言全部下沉到下列位置，覆盖面只增不减：

| 层 | 位置 | 内容 | 运行方式 |
| --- | --- | --- | --- |
| 单元 | `src/src/*.rs` 的 `#[cfg(test)]` | 曲线插值与截断、`be32` DT 读取、厂商 `iwpriv` 报文解析（首行优先、`=` 截断）、MT5700M 缓存取整与时效、传感器状态机、锁与快照编解码 | `cargo test --manifest-path src/Cargo.toml`（109 项） |
| 端到端回归 | `src/tests/regression.rs` | 模拟 sysfs 全链路：内核下限永不被自动模式压低、启动提升先于目标、并发 `apply` 让位守护进程、永不改 kernel governor、遗留 `user_space` 接管一次性修复 | 同上（39 项） |
| 契约 | `tests/test-status-contract.sh` | 见上节，前后端字段双向比对 + 遗留输出 + 真实控制周期 | CI / 手动 |
| 迁移 | `tests/test-uci-defaults.sh` | `uci-defaults` 幂等：2.x 配置升级、非法值回收、重复执行不漂移 | CI / 手动 |
| 打包 | `tests/test-packaging.sh` | Makefile/procd/acl.d/menu.d/权限与文件清单，静态检查不触发构建 | CI / 手动 |
| 内核红线 | `tests/test-kernel-safety.sh` | 生产代码只有一个 policy 写入点、不得写入 `user_space`、补丁纯增行且只删风扇档位 | CI / 手动 |
| 前端 | `tests/frontend-smoke.js` | Node `vm` 内的浏览器忠实 DOM 桩：6 个 TAB、SVG 命名空间与渲染节点、脏值校验、`apply` 而非 `restart`、TAB 复原 | `node tests/frontend-smoke.js`（220 项） |

所有层只读隔离目录与模拟 sysfs，不写真实 PWM 节点、不停风扇、不碰热保护。
桩件一律不比真实运行时更宽松：新增桩后做过反向验证（把被测代码退回错误写法确认测试 FAIL）。

`.github/workflows/ci.yml` 的 `rust`、`frontend`、`regression`、`kernel-safety` 四个 job
分别执行上表的单元/回归、前端、契约/迁移/打包、内核红线；`release.yml` 先复用 `ci.yml`，
再由官方 SDK 构建并验证包内是静态 aarch64 二进制。
