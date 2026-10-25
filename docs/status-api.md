# 控制器状态接口契约

`/usr/sbin/h5000m-fancontrol` 是唯一的实现，`/usr/sbin/h5000m-fancontrol-status`
只是转发参数的兼容包装。LuCI 页面只消费这里的 JSON，不再对文本做切割解析。

## 命令

| 命令 | 说明 | 退出码 |
|---|---|---|
| `apply`（默认） | 执行一个控制周期 | 0 成功或已排队；1 周期失败 |
| `status` | 2.x 兼容的 `key=value` 输出 | 0 |
| `status json` | 本文档描述的 JSON 契约 | 0；读取失败为 1 |
| `status live [json]` | 立即全量重采样（含厂商射频查询） | 同上 |
| `daemon` | procd 前台循环 | 0 正常停止；1 致命错误 |
| `release` | 恢复 2.0.x 遗留的 `user_space` 策略 | 0 |
| `events [n]` | 输出最近 n 条事件日志（JSON 数组） | 0 |
| `validate` | 校验配置与生效曲线 | 0 通过；1 曲线不可用 |
| `schema` | 输出下方字段路径清单 | 0 |
| `--version` / `--help` | 版本 / 用法 | 0 |

## 采样模式：为什么默认是 `cached`

守护进程每 `interval` 秒采样一次硬件并把结果写进状态快照。页面每 5 秒轮询一次
`status json`，默认走 `cached`：只读快照 + 两个廉价寄存器读（PWM 回读、转速），
**不再重复遍历 sysfs，也不再触发 `iwpriv`**。需要现场排障时再用 `status live`。

## 缺失值语义

任何不可用的数值一律为 `null`（JSON）或空串（`key=value`）。控制器不会把读取失败
变成 `0 °C` 或 `0` 输出。每个传感器另有 `status` 说明原因：

| `status` | 含义 |
|---|---|
| `ok` | 读取成功且数值在有效区间内，`value_c` 有值 |
| `missing` | 没有匹配的传感器节点，或控制器尚未发布过采样 |
| `stale` | 有缓存但已超过新鲜度上限（模组缓存默认 60 秒），或带时间戳在未来 |
| `invalid` | 节点存在但数值无法解析或超出 −40…150 °C |
| `failed` | 有界外部调用超时或失败（厂商射频查询） |

`valid` 是 `status === "ok" && value_c !== null` 的便捷布尔值；前端只在 `valid` 为真
时画数值条，否则显示斜线占位并给出 `error` 文本。

## 字段表

`h5000m-fancontrol schema` 会输出同一份清单，CI 用它和前端做双向比对
（`tests/test-status-contract.sh`）。

### 顶层

| 字段 | 类型 | 说明 |
|---|---|---|
| `schema` | int | 契约版本，当前 `3` |
| `generated_at` | int | 生成时刻（Unix 秒） |
| `sample_mode` | `"cached"` \| `"live"` | 本次数值来源 |

### `controller`

| 字段 | 类型 | 说明 |
|---|---|---|
| `version` | string | 控制器版本 |
| `health` | `"running"` \| `"failsafe"` \| `"stale"` \| `"error"` \| `"stopped"` | 综合健康状态 |
| `running` | bool | 是否有存活的守护进程持锁 |
| `pid` | int \| null | 持锁进程 PID |
| `updated_at` | int | 快照发布时间 |
| `state_age_s` | int \| null | 快照年龄 |
| `stale` | bool | 快照是否已过期（> max(3×interval, 30 s)） |
| `interval_s` | int | 配置的采样间隔 |
| `cycle_count` | int | 该进程累计周期数 |
| `consecutive_failures` | int | 连续失败次数 |
| `last_good_cycle` | int | 最近一次非失败周期的时间戳 |
| `last_error` | string \| null | 最近一次错误说明 |
| `result` | `"ok"` \| `"failsafe"` \| `"error"` \| `"none"` | 快照结论；`none` 表示尚未发布 |
| `sensor_failure` | bool | 是否所有传感器都不可用 |

### `settings`

生效配置（已校验、已限幅）。`issues[]` 逐条列出被修正的项：

```json
{ "option": "interval", "value": "900",
  "problem": "above the maximum of 60", "applied": "clamped to 60" }
```

| 字段 | 类型 |
|---|---|
| `schema_version` | int |
| `enabled` | bool |
| `mode` | `"auto"` \| `"manual"` \| `"kernel"` |
| `curve` | `"silent"` \| `"balanced"` \| `"performance"` \| `"custom"` |
| `temp_source` | `"max"` \| `"cpu"` |
| `manual_pwm`、`start_pwm`、`fail_pwm` | int 0–255 |
| `interval`、`down_delay`、`start_boost_ms` | int（秒 / 秒 / 毫秒） |
| `hysteresis` | int 0–10（°C） |
| `curve_silent` / `curve_balanced` / `curve_performance` / `curve_custom` | string |
| `curve_data` | string，当前生效的曲线文本 |
| `curve_valid` | bool |
| `curve_error` | string \| null，不可用原因（英文，前端本地化展示为原始信息） |
| `kernel_only` | bool，`enabled=false` 或 `mode=kernel` |
| `issues` | 上述对象数组 |
| `policy_signature` | string，影响控制的取值指纹 |

### `fan`

| 字段 | 类型 | 说明 |
|---|---|---|
| `pwm_path` | string \| null | 控制节点；`null` 表示未找到风扇 |
| `pwm_value` | int \| null | 寄存器当前值（**实际输出**） |
| `pwm_max` | int \| null | `pwm1_max`，仅诊断用 |
| `requested_pwm` | int | 曲线/手动请求值（未叠加下限） |
| `target_pwm` | int | 决策目标值（已叠加下限与降速延迟） |
| `applied_pwm` | int | 控制器上次写入后回读的值 |
| `requested_percent` / `target_percent` / `applied_percent` | int 0–100 |
| `rpm` | int \| null | 转速，仅当硬件提供测速节点 |
| `rpm_available` | bool | 为 `false` 时前端必须显示「无转速反馈」，不得给出推算值 |
| `fan_path` | string \| null | 测速节点路径 |
| `reason` | string | 见下表 |
| `reason_code` | string | 去掉 `+kernel-floor` 后缀的基础原因 |
| `kernel_floor_applied` | bool | 目标值是否被内核下限抬高 |
| `boost_applied` | bool | 上一周期是否使用了启动助推 |
| `down_since` | int | 降速延迟计时起点（0 表示未在延迟中） |
| `down_delay_remaining_s` | int | 降速延迟剩余秒数 |

`reason` 取值保持与 2.x 一致：`auto`、`manual`、`kernel`、`auto-down-delay`、
`sensor-failsafe`、`curve-failsafe`、`pwm-missing`、`pwm-write-failed`，命中安全下限时
追加 `+kernel-floor`；尚无快照时为 `no-snapshot`。

### `thermal`

| 字段 | 类型 | 说明 |
|---|---|---|
| `floor_pwm` | int 0–255 | 内核要求的最小输出 |
| `floor_percent` | int 0–100 |
| `state` / `max_state` | int | `pwm-fan` 冷却设备的当前需求档位与最大档位 |
| `owner` | `"shared"` \| `"userspace"` \| `"userspace-pwm"` \| `"kernel"` \| `"unknown"` | 风扇控制权归属 |
| `basis` | string | 下限判定依据（人类可读） |
| `cooling_levels` | int[] | 设备树 `cooling-levels` 表 |
| `map_states` | `{state, trigger_c}[]` | 冷却映射声明的档位与触发温度（阈值减迟滞） |
| `legacy_policy_pending` | bool | 是否存在待恢复的 2.0.x `user_space` 记录 |
| `protections` | 对象，见下 |

`owner` 语义：`shared` = 内核冷却映射仍引用风扇，双方共同决定 PWM；
`userspace` / `userspace-pwm` = 无映射引用风扇，控制器独占（此时由本控制器执行
83 °C / 113 °C 紧急下限）；`kernel` = 已禁用或 `mode=kernel`。

### `thermal.protections`

只读核对内核侧保护是否完好：

| 字段 | 类型 | 说明 |
|---|---|---|
| `zone` / `type` / `policy` | string \| null | CPU 散热区及其 governor |
| `available_policies` | string[] |
| `trips` | `{zone,name,type,temp_c,hysteresis_c}[]`，来自设备树 |
| `cpufreq_cooling` | bool | CPU 降频冷却设备是否注册 |
| `fan_cooling_device` | string \| null |
| `fan_max_state` | int |
| `intact` | bool | governor 未被接管且存在 hot 或 critical 触发点 |
| `governor_hijacked` | bool | 该散热区被切到 `user_space`（控制器自身永不这样做） |
| `warnings` | string[] |

### `control`

| 字段 | 类型 | 说明 |
|---|---|---|
| `temp_c` | int \| null | 参与曲线计算的温度 |
| `sensor_label` | string \| null | 内核给出的传感器名 |
| `sensor_source` | string \| null | 机器可读来源 |
| `source_mode` | `"max"` \| `"cpu"` |

### `sensors[]`

| 字段 | 类型 |
|---|---|
| `id` | `"cpu"` \| `"phy"` \| `"wifi1"` \| `"wifi2"` \| `"module"` |
| `title` | string |
| `value_c` | int \| null |
| `status` | 见上文 |
| `valid` | bool |
| `label` / `source` / `path` | string \| null |
| `error` | string \| null |
| `sampled_at` | int |
| `age_s` | int \| null |

五个槽位恒定存在，缺失也是显式的一行，客户端不需要猜默认值。

### `events[]`、`paths`

`events[]` 为 `{ts, level, code, message}`，来自 `/var/run/h5000m-fancontrol.events`
（有界环形日志，因此页面不需要 `logread` 权限）。`paths` 暴露
`uci_config`、`state`、`event_log`、`lock_dir`、`sysfs`，供诊断页展示。

## 稳定性约定

字段只增不删、不改语义。`schema` 递增表示破坏性变更。前端读取任何字段时都必须容忍
`null` 与新增字段。
