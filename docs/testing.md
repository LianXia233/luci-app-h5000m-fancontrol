# 测试与验证

所有测试都在隔离目录里跑，**不会**写真实 PWM 节点、不会停转真实风扇、不会改动
主机的散热区。真实 sysfs 路径只在设备上手验时才会被使用。

## 一键执行

```sh
cd src && cargo fmt --check
cd src && cargo clippy --all-targets -- -D warnings
cd src && cargo test --release                 # 单元 + 回归
node --check htdocs/luci-static/resources/view/h5000m/fancontrol.js
node tests/frontend-smoke.js                   # 前端渲染检查
node scripts/i18n-audit.js                     # 语言包覆盖率
./tests/test-status-contract.sh               # 契约 + 控制周期 + 失效路径
./tests/test-uci-defaults.sh                  # UCI 迁移与幂等
./tests/test-packaging.sh                     # 打包与密钥守卫
./tests/test-kernel-safety.sh                 # 内核热保护红线（静态守卫）
shellcheck -s sh root/etc/init.d/h5000m-fancontrol \
  root/etc/uci-defaults/91-h5000m-fancontrol-v3 \
  root/usr/sbin/h5000m-fancontrol-status tests/*.sh
shellcheck -s bash scripts/build-release.sh
jq empty root/usr/share/luci/menu.d/*.json root/usr/share/rpcd/acl.d/*.json
cd src && cargo build --release --target aarch64-unknown-linux-musl
```

GitHub Actions 的 `ci.yml` 就是上面这些，分成 `rust`、`frontend`、`regression`、
`kernel-safety` 四个 job；`kernel-safety` 跑 `tests/test-kernel-safety.sh` 与
`cargo test -- protection`，守住「不得接管 CPU 散热区」与「设备树补丁不得削弱保护」
两条红线。

## 覆盖内容

### `src/`（109 个单元测试 + 39 个回归测试）

| 位置 | 覆盖 |
|---|---|
| `curve.rs` | 端点、插值、向零截断、负数/超界/重复温度/畸形文本、点数上限 |
| `config.rs` | 出厂默认值、`off`→`kernel`、限幅并记录 issue、曲线选择、失效保护下限、策略指纹 |
| `uci.rs` | 命名/匿名段、引号与转义、`list`、重复选项、未知指令只告警 |
| `sensor.rs` | 毫度换算、−40…150 区间、畸形值、厂商报文解析（含第二个 `=`、首行优先、>125 拒绝）、模组缓存四舍五入 |
| `safety.rs` | 设备树大端属性读取、档位 >255 拒绝、所有权字符串、遗留 `user_space` 恢复（含越界路径不写入）、governor 被接管时判定保护不完整 |
| `state.rs` | 快照往返、缺失/垃圾字段保持未知、2.x 快照仍可读、重复键、陈旧判定、换行不能伪造新字段 |
| `controller.rs` | 三模式、滞回、降速延迟、失效保护不受延迟影响、下限抬高、控制来源透传 |
| `report.rs` | 健康状态优先级、未知即 `null`/空串、2.x 全部 legacy 键仍在、字段清单 |
| `lock.rs`、`events.rs`、`platform.rs`、`paths.rs`、`json.rs` | 互斥与陈旧锁回收、日志限流、有界子进程、进程存活判定、目录自然排序、JSON 转义 |
| `tests/regression.rs` | 模拟一整套 H5000M 环境（thermal zone、hwmon、pwm-fan、cooling device、设备树、模组缓存、射频接口），跑真实 `run_cycle`：原生优先、厂商回退、超时/缺工具降级、max/cpu、写寄存器、独占/共享下限、83/113 紧急档、启动助推、写失败、配置热更新、缓存不重采样、状态过期、`apply` 让位、事件日志 |

### `tests/test-status-contract.sh`

用真二进制 + 假硬件目录：

- `schema` 清单里的每个字段，前端源码里必须真的用到；前端引用的字段必须都在契约里（双向）。
- JSON 结构、未知为 `null`、`rpm_available=false` 时不给数值。
- `key=value` 输出的全部 2.x 键仍存在，缺失量为空串。
- 一次完整 `apply`：寄存器写入值与曲线算出的值一致（52 °C → 147）、快照与事件日志落盘。
- 另一个存活控制器持锁时，`apply` 只写唤醒标记、返回 `queued`、**不碰寄存器**。
- 拔掉 PWM 节点后：`status` 明确报 `pwm-missing`，`apply` 退出码非 0。

### `tests/frontend-smoke.js`（220 项）

浏览器忠实的 DOM 桩：`document.createElement()` 得到的是 **xhtml 命名空间**元素，
只有 `createElementNS(SVG_NS, …)` 才是真 SVG 元素——正是此前「图表全空白却通过断言」
那个 bug 的成因。检查内容：

- 风扇/曲线/仪表/温度条全部是真 SVG 子节点（逐个 `namespaceURI` 断言）。
- 没有节点被搬进第二个父节点（同节点复用的隐性 bug）。
- 六个 TAB、只有一个选中、面板切换、localStorage 与 `location.hash` 记忆。
- 未知读数不画数值、过期传感器画斜线占位、无测速时不出现任何 RPM 数字。
- 曲线标记 `61 °C · 66%` 与后端逐度插值一致；越界输入在保存前被拦下。
- 样式表不含 `backdrop-filter`，根节点钉住层叠上下文（不遮挡主题导航）。
- 轮询节奏 5 秒，与控制器 `interval` 解耦。

**反向验证**：把 SVG 辅助函数退回朴素的 `document.createElement` 后重跑，
182/220 项失败；改回后 220/220 通过。同理，把内核下限钳制改成 `if false`
后新增的 `automatic_control_never_undercuts_the_kernel_floor_end_to_end`
立即失败。桩件不放宽于真实运行时。

### `tests/test-kernel-safety.sh`

静态红线检查（行为部分由 `cargo test -- protection` 与回归测试承担）：

- `src/src/*.rs` 去掉 `#[cfg(test)]` 之后的代码里，`safety.rs` 只有**一个**文件写入点，
  且位于 `restore_legacy_policy`；任何模块的生产代码都不允许把字面量 `user_space`
  写进文件。
- 除 `safety.rs` 外没有模块构造 `policy` / `available_policies` 路径（读取 thermal zone
  温度不受限制）。
- 恢复路径必须保留「记录的 governor 是 `user_space` 时降级为 `step_wise`」这段逻辑，
  并且 `inspect_kernel_protection` 仍然存在。
- 设备树补丁必须**纯增行**（删除行数为 0），`/delete-node/` 只允许
  `cpu-active-high`、`cpu-active-low`、`cpu-passive`，不得给任何 trip 赋值。

**反向验证**：往 `safety.rs` 生产代码里插一个额外写入点 → 报「2 file writers」失败；
把补丁里的 `/delete-node/ cpu-passive` 改成 `cpu-thermal` → 报不是风扇档位；
往补丁里加一行删除 → 报「removes 1 lines」。三处都按预期失败后才保留该检查。

## 已在真机之外验证到的东西- `aarch64-unknown-linux-musl` 交叉编译产出 `ELF 64-bit LSB executable, ARM aarch64,
  version 1 (SYSV), statically linked, stripped`，约 580 KB，`ldd` 报「not a dynamic
  executable」。
- 发布脚本会解开生成的 APK/IPK，确认包内二进制确实是静态 aarch64，而不是宿主机产物。
- `shellcheck` 与 `jq` 由 CI 安装后执行（`frontend` job 的 `Install validation tools`）；
  宿主没有这两个工具时，本地一键执行里对应步骤会跳过或报 command not found，
  不代表检查通过。

## 仍未验证（需要 H5000M 实机）

以下为**尚未在真机上验证**的项目，不应被当作已完成：

1. 页面在设备 LuCI 主题（含深色）下的实际视觉效果、移动端是否横向溢出、
   六 TAB 的高度与留白 —— 需要真机浏览器目视确认。
2. 厂商 `iwpriv ra0/rai0 stat` 在真实 mt_wifi 驱动上的输出格式与耗时（当前仅有
   有界超时与降级路径的模拟测试）。
3. `/var/run/mt5700m/temperature` 在实机上的写入频率与新鲜度是否满足默认 60 秒。
4. 真实设备树 `pwm-fan/cooling-levels` 与 cooling-maps 的 phandle 匹配结果
   （模拟测试按常见布局构造）。
5. 长时间运行下的转速读数（`fanN_input` 是否存在取决于固件）。
6. 从 2.1.1 的 shell 控制器原地升级到本版本时，procd 换进程瞬间的 PWM 连续性。

设备验证建议顺序：先 `status live` 与 `thermal.protections` 只读确认 →
备份 `/etc/config/h5000m_fancontrol` → 装包 → `restart` → 依次验证三模式切换、
曲线生效、配置持久化、安全降级（拔传感器读数的等价手法：把
`H5000M_FAN_MT5700M_TEMP_MAX_AGE` 调小观察 failsafe）。
