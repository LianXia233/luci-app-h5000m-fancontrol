#!/bin/sh
# Regression test: control decisions and thermal safety against a simulated
# sysfs + device tree. Covers curve application, manual/kernel modes, sensor
# and curve failsafe, the kernel cooling-map floor, down-delay, start boost,
# governor preservation and concurrency. Uses H5000M_TEST_BIN only.
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
BIN="${H5000M_TEST_BIN:-$ROOT/src/target/x86_64-unknown-linux-musl/release/h5000m-fancontrol}"
if [ ! -x "$BIN" ]; then
	echo "SKIP: controller binary not built ($BIN); run cargo build first" >&2
	exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

SYS="$TMP/sys"
NET="$TMP/net"
RUN="$TMP/run"
DT="$TMP/dt"
mkdir -p "$SYS/class/thermal" "$SYS/class/hwmon" "$NET" "$RUN" "$DT" "$TMP/path"

write() {
	mkdir -p "$(dirname "$1")"
	printf '%b' "$2" > "$1"
}
write_be32() {
	mkdir -p "$(dirname "$1")"
	awk "BEGIN { for (i = 2; i < ARGC; i++) printf \"%c%c%c%c\", int(\$i/16777216)%256, int(\$i/65536)%256, int(\$i/256)%256, \$i%256 }" "$@" > "$1"
}

cat > "$TMP/path/uci" << EOF
#!/bin/sh
case "\$1 \$2 \$3" in
	"-q show h5000m_fancontrol")
		[ -f "$TMP/uci.conf" ] && cat "$TMP/uci.conf"
		exit 0 ;;
	*) exit 1 ;;
esac
EOF
chmod +x "$TMP/path/uci"
set_cfg() {
	: > "$TMP/uci.conf"
	for kv in "$@"; do
		printf "h5000m_fancontrol.settings.%s\n" "$kv" >> "$TMP/uci.conf"
	done
}

export H5000M_FAN_SENSOR_SYSFS_ROOT="$SYS"
export H5000M_FAN_PWM_SYSFS_ROOT="$SYS"
export H5000M_FAN_RADIO_SYSFS_ROOT="$NET"
export H5000M_FAN_DT_ROOT="$DT"
export H5000M_FAN_STATE_DIR="$RUN"
export H5000M_FAN_LOCK_DIR="$TMP/lock/h5000m-fancontrol.lock"
export H5000M_FAN_MT5700M_TEMP_CACHE="$TMP/none/temperature"
export H5000M_FAN_LEGACY_CACHE_DIR="$TMP/empty-tmp"
export PATH="$TMP/path:$PATH"

# Shared-ownership fixture: pwm-fan cooling device with levels
# [0, 96, 160, 255] and a kernel map demanding state 2 above 83 degrees.
write "$SYS/class/hwmon/hwmon0/name" 'pwmfan\n'
PWM="$SYS/class/hwmon/hwmon0/pwm1"
write "$PWM" '0\n'
write "$SYS/class/thermal/thermal_zone0/type" 'soc_thermal\n'
write "$SYS/class/thermal/thermal_zone0/temp" '55000\n'
write "$SYS/class/thermal/thermal_zone0/policy" 'step_wise\n'
write "$SYS/class/thermal/thermal_zone0/available_policies" 'step_wise user_space\n'
write "$SYS/class/thermal/cooling_device0/type" 'pwm-fan\n'
write "$SYS/class/thermal/cooling_device0/max_state" '3\n'
write_be32 "$DT/pwm-fan/phandle" 42
write_be32 "$DT/pwm-fan/cooling-levels" 0 96 160 255
write_be32 "$DT/thermal-zones/cpu-thermal/trips/trip-active/phandle" 17
write_be32 "$DT/thermal-zones/cpu-thermal/trips/trip-active/temperature" 83000
write_be32 "$DT/thermal-zones/cpu-thermal/trips/trip-active/hysteresis" 0
write_be32 "$DT/thermal-zones/cpu-thermal/cooling-maps/map0/cooling-device" 42 2
write_be32 "$DT/thermal-zones/cpu-thermal/cooling-maps/map0/trip" 17

pwm_value() { cat "$PWM"; }

# --- 1. auto mode applies the balanced curve -------------------------------
set_cfg
"$BIN" apply
# 55 degrees is the 55:60 point -> 60% -> 153.
test "$(pwm_value)" = "153"
"$BIN" status | grep -qx 'reason=auto'
"$BIN" status | grep -qx 'thermal_owner=shared'
"$BIN" status | grep -qx 'applied_pwm=153'

# --- 2. kernel floor raises a low curve output -----------------------------
write "$SYS/class/thermal/thermal_zone0/temp" '90000\n'
set_cfg "curve='silent'" "curve_silent='20:0,50:20,95:30'"
"$BIN" apply
# 90 degrees -> 28% -> 71 on the curve, but the kernel map demands 160.
test "$(pwm_value)" = "160"
"$BIN" status | grep -qx 'reason=auto+kernel-floor'
"$BIN" status | grep -qx 'requested_pwm=71'

# --- 3. manual mode ---------------------------------------------------------
set_cfg "mode='manual'" "manual_pwm='200'"
"$BIN" apply
test "$(pwm_value)" = "200"
"$BIN" status | grep -qx 'reason=manual'

# --- 4. kernel mode leaves the output alone --------------------------------
set_cfg "mode='kernel'"
write "$PWM" '180\n'
"$BIN" apply
test "$(pwm_value)" = "180"
"$BIN" status | grep -qx 'thermal_owner=kernel'
# ... and the CPU governor is untouched.
test "$(cat "$SYS/class/thermal/thermal_zone0/policy")" = "step_wise"

# --- 5. sensor failsafe drives a high output -------------------------------
set_cfg "mode='auto'" "fail_pwm='220'"
rm "$SYS/class/thermal/thermal_zone0/temp"
"$BIN" apply
# Missing CPU reading under shared ownership escalates to the full floor.
test "$(pwm_value)" = "255"
"$BIN" status | grep -qx 'result=failsafe'
"$BIN" status | grep -qx 'reason=sensor-failsafe+kernel-floor'
write "$SYS/class/thermal/thermal_zone0/temp" '55000\n'
"$BIN" apply
"$BIN" status | grep -qx 'result=ok'   # recovery is logged

# --- 6. invalid curve failsafe ---------------------------------------------
set_cfg "curve='custom'" "curve_custom='40:50,30:60'"
"$BIN" apply
test "$(pwm_value)" = "255"
"$BIN" status | grep -qx 'reason=curve-failsafe'
set_cfg

# --- 7. down delay holds reductions ----------------------------------------
write "$SYS/class/thermal/thermal_zone0/temp" '60000\n'
set_cfg "down_delay='30'" "hysteresis='2'"
"$BIN" apply
v1="$(pwm_value)"   # 65% -> 165
write "$SYS/class/thermal/thermal_zone0/temp" '45000\n'
"$BIN" apply
test "$(pwm_value)" = "$v1"   # held
"$BIN" status | grep -qx 'reason=auto-down-delay'

# --- 8. start boost when the fan was stopped -------------------------------
set_cfg "start_pwm='240'" "start_boost_ms='700'"
write "$PWM" '0\n'
"$BIN" apply >/dev/null   # boost sleeps ~1s then settles at the curve target
final="$(pwm_value)"
test "$final" -gt 0

# --- 9. legacy policy file is repaired, never switched to user_space ------
printf '%s\nstep_wise\n' "$SYS/class/thermal/thermal_zone0" > "$RUN/h5000m-fancontrol.policy"
write "$SYS/class/thermal/thermal_zone0/policy" 'user_space\n'
"$BIN" apply
test "$(cat "$SYS/class/thermal/thermal_zone0/policy")" = "step_wise"
test ! -f "$RUN/h5000m-fancontrol.policy"

# --- 10. concurrent applies never corrupt the output -----------------------
set_cfg "manual_pwm='128'" "mode='manual'"
write "$PWM" '128\n'
( "$BIN" apply 2>/dev/null || true ) & ( "$BIN" apply 2>/dev/null || true ) & wait
v="$(pwm_value)"
test "$v" = "128"
"$BIN" apply
test "$(pwm_value)" = "128"

# --- 11. missing PWM node reports an error state ---------------------------
mv "$PWM" "$PWM.bak"
if "$BIN" apply 2>/dev/null; then
	echo 'apply should fail without a PWM node' >&2
	exit 1
fi
"$BIN" status | grep -qx 'result=error'
"$BIN" status | grep -qx 'reason=pwm-not-found'
mv "$PWM.bak" "$PWM"

echo 'PASS: modes, curves, failsafe, kernel floor, down delay, boost, governor repair, concurrency'
