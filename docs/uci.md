# UCI 配置参考

配置文件 `/etc/config/h5000m_fancontrol`，唯一段 `settings`。控制器每周期直接
重新解析该文件（不 spawn `uci`），因此 `uci commit` 之后最多一个 `interval` 秒即生效；
`uci-defaults` 与页面保存还会主动唤醒守护进程立即执行一次。

`/etc/config/h5000m_fancontrol` 在 `Makefile` 中声明为 conffile，升级时保留。

| 选项 | 类型 | 默认 | 范围 | 生效方式 |
|---|---|---|---|---|
| `schema_version` | int | 3 | — | 迁移脚本写入，控制器只读 |
| `enabled` | 0/1 | 1 | 0、1 | 0 时等价 `mode=kernel`，不写 PWM |
| `mode` | 枚举 | `auto` | `auto`/`manual`/`kernel`（历史值 `off` 自动映射为 `kernel`） | 立即 |
| `curve` | 枚举 | `balanced` | `silent`/`balanced`/`performance`/`custom` | 立即（仅自动模式） |
| `temp_source` | 枚举 | `max` | `max`/`cpu` | 立即 |
| `manual_pwm` | int | 160 | 0–255 | 立即（手动模式） |
| `interval` | int | 5 | 2–60 秒 | 下一周期起 |
| `hysteresis` | int | 2 | 0–10 °C | 立即（只影响降速） |
| `down_delay` | int | 30 | 0–300 秒 | 立即 |
| `start_pwm` | int | 192 | 80–255 | 下次启动助推 |
| `start_boost_ms` | int | 700 | 0–3000 毫秒 | 下次启动助推 |
| `fail_pwm` | int | 255 | 192–255 | 立即 |
| `curve_silent` | 曲线 | `20:0,35:0,40:50,55:50,70:60,85:75,95:100` | 见下 | 立即 |
| `curve_balanced` | 曲线 | `20:0,35:0,40:50,55:60,70:75,85:90,95:100` | 见下 | 立即 |
| `curve_performance` | 曲线 | `20:0,30:60,45:70,60:82,75:92,85:100` | 见下 | 立即 |
| `curve_custom` | 曲线 | `20:0,40:50,55:60,70:75,85:90,95:100` | 见下 | 立即 |

## 曲线格式

`温度:百分比` 用逗号分隔，例如 `40:50,70:75`。

- 温度与百分比都必须是非负整数；温度 ≤ 150，百分比 ≤ 100。
- 温度必须**严格递增**（重复或下降即整条曲线无效）。
- 至少 2 个点，最多 32 个点。
- 区间内线性插值，整数除法向零截断；低于首点取首点，高于末点取末点。
- 曲线无效时控制器转入 `curve-failsafe` 输出 `fail_pwm`，**不会**当成 0% 停转。

## 校验与容错

非法取值不会让控制器拒跑：数值超出范围会被限幅、无法解析会退回默认值、非法枚举退回
安全默认（`mode=auto`、`curve=balanced`、`temp_source=max`）。每一次修正都会出现在
`status json` 的 `settings.issues[]` 与页面「诊断与日志 → 配置检查」里，
也可以用 `h5000m-fancontrol validate` 在脚本里检查（曲线不可用时退出码为 1）。

## 升级迁移

`/etc/uci-defaults/91-h5000m-fancontrol-v3` 幂等执行：

- 把历史值 `mode=off` 改为 `kernel`；
- 只为**缺失**的选项补默认值，已配置的 `enabled`、`curve`、`manual_pwm` 以及所有
  2.x 选项都保持原值不动；
- 删除 1.x 遗留的 `min_pwm`、`max_pwm`、`low_temp`、`high_temp`；
- 写入 `schema_version=3` 并 `commit`；
- `enable` 并 `restart` 服务，让同一位置上的原生二进制接管旧的 shell 控制器。

因为以 `schema_version < 3` 为门槛，重复执行是空操作；`tests/test-uci-defaults.sh`
用桩 `uci` 验证了这一行为（1.x 迁移、幂等、2.x 取值保持不变）。

## 手工操作

```sh
/usr/sbin/h5000m-fancontrol status          # 2.x 兼容的 key=value
/usr/sbin/h5000m-fancontrol status json     # 机器可读契约
/usr/sbin/h5000m-fancontrol status live     # 立即全量重采样
/usr/sbin/h5000m-fancontrol validate        # 配置与曲线检查
/usr/sbin/h5000m-fancontrol events 20       # 最近 20 条事件
/usr/sbin/h5000m-fancontrol apply           # 立即执行一个控制周期
/etc/init.d/h5000m-fancontrol restart       # 需要时才重启服务
```
