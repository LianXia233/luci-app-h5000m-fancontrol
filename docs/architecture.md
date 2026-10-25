# 架构与模块职责

原生控制器位于 `src/`，是一个零外部依赖的 Rust crate（`[dependencies]` 为空），
编译产物安装为 `/usr/sbin/h5000m-fancontrol`。功能对照与逐条移植依据见
[refactor-mapping.md](refactor-mapping.md)，对外数据接口见
[status-api.md](status-api.md)。

## 为什么零依赖

控制器在启动早期就以 root 常驻，负责散热安全。多一个 crate 就多一份供应链与体积，
也多一个「设备上没有」的失败面。JSON 写入、信号处理、有界子进程、目录枚举都在
`std` 之内自行实现，`Cargo.lock` 里只有本 crate 自己，因此 `--offline` 构建可复现。

## 模块

| 模块 | 职责 | 关键点 |
|---|---|---|
| `paths` | 所有文件系统根位置 | 全部可用环境变量覆盖，见「测试接口」；目录枚举按尾部数字自然排序 |
| `uci` | 直接解析 `/etc/config/h5000m_fancontrol` | 不再每周期 spawn `uci`；支持引号、转义、`list`、续行与匿名段 |
| `config` | 类型化配置：默认值、范围、遗留值迁移 | 每次修正都记为 `ConfigIssue` 对外可见；`policy_signature()` 标识影响控制的取值 |
| `curve` | 曲线解析、校验、插值 | 与 2.x 逐位一致的整数运算（向零截断） |
| `sensor` | thermal zone / hwmon 发现、读数、模组缓存、厂商射频回退 | 陈旧与无效一律「未知」；回退有 1 秒硬超时 |
| `fan` | PWM 与测速节点发现、读写、冷却设备 | `pwmfan` 名称优先于任意的 `pwm1` |
| `safety` | 设备树冷却映射 → 内核安全下限；CPU 散热区与内核保护只读核对；历史策略恢复 | `cur_state` 不能当下限（直接写 PWM 也会改它）；永不把散热区切到 `user_space` |
| `controller` | 决策 + 执行 | `decide()` 为纯函数；`actuate()` 是唯一写 PWM 之处 |
| `state` | `/var/run` 状态快照编解码 | 原子写（临时文件 + rename）；严格解析，绝不猜测 0 |
| `report` | 统一状态模型 + JSON / `key=value` 两种序列化 | `contract()` 由写入器自身导出字段清单，供 CI 比对 |
| `events` | 有界事件环形日志 + 重复消息抑制 | 页面因此不需要 `logread` 权限 |
| `lock` | 排他控制锁 | 沿用 2.x 目录与 `pid` 文件，新旧实现互斥 |
| `daemon` | procd 前台循环、唤醒、`apply` 让位 | 锁贯穿整个进程生命周期 |
| `platform` | 信号、进程存活、有界子进程、时钟 | 非 Linux 分支只用于让测试在任意主机上可编译运行 |
| `logging` | 直接写 `/dev/log`，失败回落 stderr | 不 fork `logger` |
| `json` | 无依赖 JSON 值树与写出器 | `null` 与整数的区分是契约的基础 |
| `cli` | 参数解析与命令分派 | 保留 2.x 的命令、语义与退出码 |

## 数据流

```
procd └─ h5000m-fancontrol daemon
        └─ daemon::run  ── 取锁（贯穿进程生命周期）→ 恢复遗留策略（若有）
             │
             │  每个周期（或被 SIGHUP/SIGUSR1、唤醒标记、配置变更、SIGTERM 提前打断）
             ▼
           controller::run_cycle
             ├─ config::Settings::load        （解析 UCI 文件，记录修正）
             ├─ fan::discover                 （PWM/测速/冷却设备）
             ├─ sensor::sample_all            （thermal + hwmon + 厂商回退 + 模组缓存）
             ├─ safety::evaluate_floor        （设备树冷却映射 → 下限）
             ├─ controller::decide            （纯函数：模式/曲线/滞回/降速延迟/失效保护）
             ├─ controller::actuate           （启动助推 → 写 PWM → 回读校验）
             └─ state::Snapshot               （原子发布）
                   ▲
LuCI 页面 ── fs.exec("h5000m-fancontrol-status json") ── report::build(cached)
              只读快照 + PWM/转速两个廉价寄存器读，不重复采样硬件
```

## 并发与生命周期

- **单一写入者**：守护进程在启动时取锁并在整个生命周期持有。`apply` 若发现锁被存活
  进程持有，则写唤醒标记并返回 0（`queued`），不再出现 2.x 中 `apply` 与循环在周期间隙
  互相抢锁、并发写 PWM 的窗口。
- **陈旧锁回收**：`pid` 无法解析、进程不存在或 `/proc/<pid>/cmdline` 不含
  `h5000m-fancontrol` 时回收；无法读取 cmdline 时按「存活」处理，宁可等待也不抢锁。
- **配置热更新**：每周期重新解析 UCI；睡眠切片中检查 `/etc/config` 的 mtime 与唤醒
  标记，因此保存即生效。`/etc/init.d/... reload` 走 `procd_send_signal ... HUP`，
  **不再 stop+start**。
- **退出语义**：`SIGTERM`/`SIGINT` 停止循环、释放锁并返回 0；不主动把 PWM 清零，
  因为内核冷却映射仍然在管理风扇，强行归零才是危险动作。
- **不遗留错误状态**：启动助推期间的写入都会落到目标值；若写入失败，本周期以
  `pwm-write-failed` 记录错误并在快照中保留 `last_error`。

## 测试接口

| 变量 | 默认 | 用途 |
|---|---|---|
| `H5000M_FAN_SENSOR_SYSFS_ROOT` | `/sys` | thermal zone 与 hwmon 根 |
| `H5000M_FAN_RADIO_SYSFS_ROOT` | `/sys/class/net` | 判断射频接口是否存在 |
| `H5000M_FAN_DT_ROOT` | `/sys/firmware/devicetree/base` | 设备树属性 |
| `H5000M_FAN_UCI_CONFIG` | `/etc/config/h5000m_fancontrol` | 配置文件 |
| `H5000M_FAN_RUN_DIR` | `/var/run` | 快照、事件、唤醒标记 |
| `H5000M_FAN_LOCK_DIR` | `/var/lock/h5000m-fancontrol.lock` | 控制锁 |
| `H5000M_FAN_PROC_ROOT` | `/proc` | 进程存活判断 |
| `H5000M_FAN_MT5700M_TEMP_CACHE` | `/var/run/mt5700m/temperature` | 模组温度缓存 |
| `H5000M_FAN_MT5700M_TEMP_MAX_AGE` | `60` | 缓存新鲜度上限（秒） |
| `H5000M_FAN_LEGACY_CACHE_DIR` | `/tmp` | 旧版 QModem 缓存扫描目录 |
| `H5000M_FAN_SYSLOG_SOCKET` | `/dev/log` | syslog 套接字 |

前四个变量名与 2.x 完全一致，已有的传感器测试脚本无需改名即可复用。
