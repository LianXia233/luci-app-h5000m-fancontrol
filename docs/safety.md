# 温控安全策略

风扇控制直接决定 H5000M 的散热。本文是「什么情况下风扇会怎么动」的完整说明，
所有结论都能在 `src/src/controller.rs`、`src/src/safety.rs` 与
`src/tests/regression.rs` 中找到对应实现与测试。

## 三条不变式

1. **内核的 CPU 保护始终有效。** 控制器只写 PWM，永不把 CPU 散热区的 policy 切成
   `user_space`，也从不修改任何触发点。它甚至会在启动时把 2.0.x 曾切到
   `user_space` 的散热区恢复为记录的原 governor（记录不可用时退回 `step_wise`），
   并且**绝不会**把 `user_space` 写回去。
   `status json` 的 `thermal.protections` 只读上报 policy、`cpufreq` 冷却设备是否
   注册、设备树里的 passive/hot/critical 触发点，以及 `intact` 结论。
2. **温度缺失就是未知。** 读取失败、超出 −40…150 °C、缓存过期、时间戳在未来、
   厂商查询超时或返回非法值，一律记为 `missing`/`invalid`/`stale`/`failed` 且
   `value_c = null`，绝不折算成 0 °C，也绝不参与决策。
3. **没有可靠输入时风扇偏高速。** 没有任何有效传感器 → `sensor-failsafe`；
   生效曲线不可解析 → `curve-failsafe`；两者都输出 `fail_pwm`（默认 255，
   允许范围 192–255，即始终高于 75%）。

## 决策顺序（每个控制周期）

1. `enabled=0` 或 `mode=kernel`：**不写 PWM**，读取当前寄存器值作为目标与实际输出，
   `reason=kernel`、`thermal_owner=kernel`。内核冷却映射继续驱动风扇。
2. `mode=manual`：`requested = manual_pwm`。
3. `mode=auto` 且控制温度不可用：`requested = fail_pwm`，进入 `sensor-failsafe`。
4. `mode=auto` 且曲线无效：`requested = fail_pwm`，进入 `curve-failsafe`。
5. 正常自动：`requested = curve(控制温度)`。
6. `target = max(requested, 内核安全下限)`；被抬高时 `kernel_floor_applied=true`，
   `reason` 追加 `+kernel-floor`。
7. 仅自动模式且结果健康时才允许降速：若 `target < 上次已写入值`，用
   `curve(控制温度 + hysteresis)` 重算候选（滞回让降速更保守），并再次不低于下限；
   随后若距 `down_since` 不足 `down_delay` 秒，则**维持上一次输出**并记
   `reason=auto-down-delay`。温度上升永远立即生效。
8. 启动助推：当前寄存器为 0 且目标 > 0 且 `start_boost_ms > 0` 时，先写
   `max(start_pwm, target)`，等待精确毫秒数，再写目标值。
9. 只有 `current != target` 才写 PWM；写入后回读，回读不符（驱动限幅）如实上报
   `last_error`，不谎报成功。

## 内核安全下限怎么算

直接写 PWM 也会改变 `cur_state`，所以**不能**把 `cur_state` 当下限。下限来自设备树：

1. 找到 `pwm-fan` 节点的 `phandle` 与 `cooling-levels` 表。
2. 遍历 `thermal-zones/*/cooling-maps/*`，凡是引用风扇 `phandle` 的映射，取出
   档位与对应 trip 的 `temperature - hysteresis`。
3. CPU 温度 ≥ 该阈值时取最大档位，下限 = `cooling-levels[档位]`（>255 视为无效）。
4. 所有权判定：
   - 存在映射且引用了风扇 → `shared`（内核与控制器同时调速，界面会说明请求值与
     实际值可能不同）；
   - 存在映射但没有一个引用风扇（即应用了本仓库补丁）→ `userspace` 独占，此时
     由控制器执行紧急下限：CPU 未知 → 档位 3 且满速；≥ 83 °C → 档位 2；
     ≥ 113 °C → 档位 3；
   - `shared`/独占下若 CPU 温度读不到且存在映射 → 直接按满速下限（无法计算内核
     需求时保守取最大）；
   - 设备树完全不可读 → 回退到旧固件阈值 38/83/113 °C 选档，档位取到值就用，
     取不到就满速。

83、113、38 这三个阈值是从 2.x 逐条继承下来的经验值（对应 H5000M 固件里
`cpu-thermal` 的 active/hot 档位温度），不是本次新造的规格；它们只在**设备树冷却映射
读不出来**时作为保守回退使用，能读到设备树时一律以设备树自己声明的阈值和
`cooling-levels` 为准。补丁的适用条件与影响见 `openwrt-patches/` 与 `docs/build.md`。

## 转速与实际输出

- 界面同时给出**请求值**、**目标值**、**控制器上次写入值**与**寄存器回读值**，
  四者不同时正好说明内核或驱动在介入，这是设计意图而非 bug。
- 本机型固件不提供 `fanN_input` 测速节点，状态契约仍发布 `fan.rpm` /
  `fan.rpm_available`（供脚本消费，恒为 `null`/`false`），但界面不再渲染
  转速卡片；任何情况下都不会用 PWM 占空比反推 RPM、dBA 或风量。
- 风扇涡轮示意图的旋转动画与 270° 进度环只反映已应用的输出档位，SVG 的
  `desc` 与 `aria-label` 都明确它不是实测转速。

## 前端颜色区间

温度条的颜色分段（正常/关注/过热/危险）是**本项目的参考评估**：
CPU 70/85/95 °C、PHY 70/80/90 °C、射频 75/88/95 °C、5G 模组 70/82/90 °C，
与出厂曲线的拐点位置一致。页面上的说明文字与本文都明确它不是厂商公布的热规格；
真正的硬件保护仍然是内核的 `hot`/`critical` 触发点。

## 已知代价

`panic = "abort"`：控制器不会静默吞掉缺陷，异常直接终止进程，procd 以
`respawn 3600 5 5` 拉起，锁在退出时释放，风扇保持最后一次输出（内核仍在管理）。
控制路径本身不使用 `unwrap`/索引越界等可 panic 构造，并有回归测试覆盖。
