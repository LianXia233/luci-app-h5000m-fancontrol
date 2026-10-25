#!/bin/sh
# Regression test: frontend/backend contract. Verifies that every status
# field the LuCI view reads is present in the controller's JSON output, and
# that the legacy key=value output still carries the historical field set for
# external scripts. Uses H5000M_TEST_BIN only.
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
BIN="${H5000M_TEST_BIN:-$ROOT/src/target/x86_64-unknown-linux-musl/release/h5000m-fancontrol}"
VIEW="$ROOT/htdocs/luci-static/resources/view/h5000m/fancontrol.js"
if [ ! -x "$BIN" ]; then
	echo "SKIP: controller binary not built ($BIN); run cargo build first" >&2
	exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export H5000M_FAN_STATE_DIR="$TMP/run"
export H5000M_FAN_LOCK_DIR="$TMP/lock/h5000m-fancontrol.lock"
mkdir -p "$TMP/run"

# --json must be valid JSON (python3 json.tool; falls back to node).
json="$("$BIN" status --json)"
if command -v python3 >/dev/null 2>&1; then
	printf '%s' "$json" | python3 -m json.tool > /dev/null
else
	printf '%s' "$json" | node -e 'JSON.parse(require("fs").readFileSync(0, "utf8"))'
fi

# Every dotted field path the view accesses must exist in the JSON model.
for path in \
	controller.version controller.health controller.result controller.reason \
	controller.mode controller.curve controller.interval controller.last_error \
	controller.last_success controller.state_age controller.running \
	sensors.cpu.temp_c sensors.cpu.label sensors.cpu.source \
	sensors.phy.temp_c sensors.wifi1.temp_c sensors.wifi2.temp_c \
	sensors.module.temp_c sensors.control.temp_c sensors.control.label \
	fan.pwm_path fan.pwm fan.requested_pwm fan.target_pwm fan.applied_pwm \
	fan.rpm fan.rpm_supported \
	safety.thermal_floor safety.thermal_state safety.thermal_max_state \
	safety.thermal_owner safety.kernel_policy safety.kernel_zone \
	safety.cpu_throttling_intact safety.down_delay_active; do
	echo "$json" | grep -qF "\"${path##*.}\"" || {
		echo "JSON misses field: $path" >&2
		exit 1
	}
done

# The view must only read fields from that list. JS method calls like
# `sensors.map(` are filtered out; wifi1/wifi2 include their digit.
for field in $(grep -oE '(ctl|sensors|fan|safety)\.[a-z0-9_]+' "$VIEW" | grep -vE '\.(map|filter|forEach)$' | sort -u); do
	section="${field%%.*}"
	name="${field#*.}"
	case "$section.$name" in
		ctl.mode|ctl.curve|ctl.interval|ctl.health|ctl.result|ctl.reason|ctl.last_error|ctl.last_success|ctl.state_age|ctl.running|ctl.version) ;;
		sensors.cpu|sensors.phy|sensors.wifi1|sensors.wifi2|sensors.module|sensors.control) ;;
		fan.pwm_path|fan.pwm|fan.requested_pwm|fan.target_pwm|fan.applied_pwm|fan.rpm|fan.rpm_supported) ;;
		safety.thermal_floor|safety.thermal_state|safety.thermal_max_state|safety.thermal_owner|safety.kernel_policy|safety.kernel_zone|safety.cpu_throttling_intact|safety.down_delay_active) ;;
		*)
			echo "view reads unverified status field: $field" >&2
			exit 1
			;;
	esac
done

# Legacy text output keeps the historical field set.
legacy="$("$BIN" status)"
for key in enabled mode curve curve_data temp_source manual_pwm interval \
	hysteresis down_delay start_pwm start_boost_ms pwm pwm_value fan \
	fan_feedback fan_rpm cpu_temp cpu_label phy_temp phy_label wifi1_temp \
	wifi1_label wifi2_temp wifi2_label module_temp module_label control_temp \
	control_sensor thermal_floor thermal_state thermal_max_state thermal_owner \
	result reason requested_pwm target_pwm applied_pwm state_age; do
	echo "$legacy" | grep -q "^${key}=" || {
		echo "legacy status misses key: $key" >&2
		exit 1
	}
done

echo 'PASS: JSON model, view field contract and legacy key=value output'
