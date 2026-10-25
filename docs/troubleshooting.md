# 故障排查

先看页面「诊断与日志」，或直接执行 `h5000m-fancontrol status json`。
`controller.last_error`、`sensors[].error`、`settings.issues[]`、
`thermal.protections.warnings[]` 与 `events[]` 基本能覆盖所有情况。

## 页面提示「控制器未运行」

```sh
/etc/init.d/h5000m-fancontrol enabled        # 是否开机自启
/etc/init.d/h5000m-fancontrol start
logread -e h5000m-fancontrol | tail -50
ls -l /var/lock/h5000m-fancontrol.lock       # 谁持锁
```

锁目录里 `pid` 指向的进程若已不存在，下一次启动会自动回收锁并记录
「stale lock reclaimed」事件；若始终抢不到锁，说明还有一个控制器在跑
（升级过程中新旧二进制的同名路径替换未生效，`ps | grep h5000m-fancontrol` 确认）。

## 状态一直显示「已过期」

守护进程没在跑，或卡在某个不可中断的读取上。控制器对厂商射频查询有 1 秒硬超时，
对 `/proc`、sysfs 一律是普通文件读，不会长阻塞。

```sh
/usr/sbin/h5000m-fancontrol status live      # 手动全量采样一次
grep updated /var/run/h5000m-fancontrol.state
```

`state_age_s` 超过 `max(3×interval, 30)` 秒即为过期。

## 「未找到风扇控制节点」（`reason=pwm-missing`）

```sh
for h in /sys/class/hwmon/hwmon*; do printf '%s %s\n' "$h" "$(cat $h/name 2>/dev/null)"; done
ls /sys/class/hwmon/hwmon*/pwm1
```

- 没有任何 `pwm1`：`pwm-fan` 驱动未加载，安装 `kmod-hwmon-pwmfan`（软件包已声明依赖）。
- 有 `pwm1` 但没有名为 `pwmfan` 的芯片：控制器会退回第一个 `pwm1`；若那个 `pwm1`
  属于别的芯片，请把真正的风扇芯片命名为 `pwmfan`（设备树驱动别名）。
- 目录存在但写入失败（`reason=pwm-write-failed`）：检查该 hwmon 的 `pwm1` 是否被
  其他程序占用或变成只读；`last_error` 会带具体 errno。

## 温度全是「未知」

```sh
cat /sys/class/thermal/thermal_zone*/type /sys/class/thermal/thermal_zone*/temp
cat /sys/class/hwmon/hwmon*/name
ls -l /var/run/mt5700m/temperature
```

- CPU：需要某个 `thermal_zone*` 的 `type` 含 `cpu`/`soc`/`mt798`/`thermal`，或同名
  hwmon 芯片带 `temp1_input`。
- 射频：优先 hwmon（`mt799*`/`wifi`/`radio`）。只有该槽位为空时才会执行
  `iwpriv ra0|rai0 stat`；设备上没有 `iwpriv` 时页面会显示
  「iwpriv is not installed」并把它当作不可用，不影响其他传感器。
- 5G 模组：读 `/var/run/mt5700m/temperature` 的 `temperature=` 与 `updated=`；
  超过 `H5000M_FAN_MT5700M_TEMP_MAX_AGE`（默认 60 秒）即判为过期，回退扫描
  `/tmp/cache_base_info_*`、`/tmp/cache_*info*`。**过期与缺失都不会被当成 0 °C**。
- 全部不可用时控制器会输出 `fail_pwm`（默认 255）散热，这是有意为之。

## 风扇转速和设定值不一致

看 `thermal.owner`：

- `shared`：冷却映射仍引用风扇，内核 governor 会在需要时抬高 PWM，界面会显示
  「已被内核下限抬高」。想让自动曲线完全接管，需按 `docs/build.md` 编译固件时应用
  `openwrt-patches/h5000m-userspace-fan-control.patch`（只删风扇 cooling-map，
  CPU 降频与 hot/critical 保留）。
- `userspace` / `userspace-pwm`：独占模式，差异只可能来自 `pwm1_max` 之类的驱动限幅，
  此时 `applied_pwm` 与回读值的差会出现在 `last_error` 中。

## `thermal.protections.intact` 为 false

这是**硬件保护被削弱**的信号，必须处理：

```sh
cat /sys/class/thermal/thermal_zone*/policy          # 出现 user_space 就是问题
ls /sys/class/thermal/cooling_device*/type | xargs -I{} sh -c 'printf "{} "; cat {}/type'
```

控制器自身永不写 `user_space`。若某次手动调试改过，或系统上还有别的程序在接管散热区：

```sh
/usr/sbin/h5000m-fancontrol release     # 恢复 2.0.x 遗留记录里的 governor
```

`release` 只处理 `/var/run/h5000m-fancontrol.policy` 记录过的散热区，且路径必须位于
sysfs 根之下。若设备树里没有 `hot`/`critical` 触发点，那是固件问题，插件不会代偿。

## 保存了但没生效

```sh
h5000m-fancontrol validate              # 看是否被限幅/退回默认
h5000m-fancontrol events 20
h5000m-fancontrol apply
```

页面保存走 `uci commit` + 立即 `apply`；`uci-defaults` 或服务被禁用时会退化为
「下个周期生效」。若 `settings.policy_signature` 未变（只改了不影响控制的取值），
页面会跳过 `apply`，这是刻意为之，避免频繁扰动控制器。

## 采集开销与轮询

页面每 5 秒轮询一次，走缓存快照：不再遍历 sysfs、不再 fork 任何进程。
控制器的采样频率由 `interval` 决定，与页面刷新无关。日志与事件按「状态跃迁」写入，
重复错误由限流器合并，不会刷屏。
