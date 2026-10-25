#!/bin/sh
# Contract test: the JSON the controller publishes and the fields the LuCI view
# reads must stay in step, and the historical key=value output must keep
# carrying every 2.x key. Both directions are checked, so neither side can drop
# a field silently.
#
# Everything runs against a sandboxed set of filesystem roots; no real sysfs,
# PWM node or thermal zone is ever touched.

set -eu

# No CDPATH inheritance: `cd` must resolve relative paths from the filesystem.
CDPATH=''
ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
cd "${ROOT}"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT INT TERM

TARGET_DIR="${TMP}/cargo"
export CARGO_TARGET_DIR="${TARGET_DIR}"

say() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

say 'building the controller for the test host'
cargo build --quiet --release --manifest-path src/Cargo.toml --target-dir "${TARGET_DIR}"
BIN="${TARGET_DIR}/release/h5000m-fancontrol"
[ -x "${BIN}" ] || fail "binary missing at ${BIN}"

mkdir -p "${TMP}/sys/class/thermal/thermal_zone0" \
	"${TMP}/sys/class/hwmon/hwmon1" \
	"${TMP}/etc/config" "${TMP}/var/run" "${TMP}/var/lock" \
	"${TMP}/sys/firmware/devicetree/base/pwm-fan" \
	"${TMP}/sys/firmware/devicetree/base/thermal-zones/cpu-thermal/trips"

printf 'cpu-thermal\n' > "${TMP}/sys/class/thermal/thermal_zone0/type"
printf '52000\n' > "${TMP}/sys/class/thermal/thermal_zone0/temp"
printf 'step_wise\n' > "${TMP}/sys/class/thermal/thermal_zone0/policy"
printf 'step_wise user_space\n' > "${TMP}/sys/class/thermal/thermal_zone0/available_policies"
printf 'pwmfan\n' > "${TMP}/sys/class/hwmon/hwmon1/name"
printf '77\n' > "${TMP}/sys/class/hwmon/hwmon1/pwm1"

: > "${TMP}/etc/config/h5000m_fancontrol"

export H5000M_FAN_SENSOR_SYSFS_ROOT="${TMP}/sys"
export H5000M_FAN_RADIO_SYSFS_ROOT="${TMP}/sys/class/net"
export H5000M_FAN_DT_ROOT="${TMP}/sys/firmware/devicetree/base"
export H5000M_FAN_UCI_CONFIG="${TMP}/etc/config/h5000m_fancontrol"
export H5000M_FAN_RUN_DIR="${TMP}/var/run"
export H5000M_FAN_LOCK_DIR="${TMP}/var/lock/h5000m-fancontrol.lock"
export H5000M_FAN_PROC_ROOT="${TMP}/proc"
export H5000M_FAN_MT5700M_TEMP_CACHE="${TMP}/var/run/mt5700m/temperature"
# The contract tests run without the mt5700 manager: keep the bridge off so the
# controller never dials a real loopback service while testing.
export H5000M_FAN_MT5700M_RPC_PORT=0

VIEW='htdocs/luci-static/resources/view/h5000m/fancontrol.js'
[ -f "${VIEW}" ] || fail 'the LuCI view is missing'

# Every field the page consumes, in contract notation.
CONTRACT_FIELDS='
controller.health
controller.running
controller.version
controller.cycle_count
controller.consecutive_failures
controller.last_error
controller.last_good_cycle
controller.state_age_s
controller.interval_s
controller.result
controller.sensor_failure
settings.mode
settings.curve
settings.curve_data
settings.curve_valid
settings.issues
settings.temp_source
settings.manual_pwm
settings.interval
settings.hysteresis
settings.down_delay
settings.start_pwm
settings.start_boost_ms
settings.fail_pwm
settings.schema_version
settings.kernel_only
fan.pwm_path
fan.pwm_value
fan.requested_pwm
fan.target_pwm
fan.applied_pwm
fan.rpm
fan.rpm_available
fan.reason
fan.kernel_floor_applied
fan.boost_applied
fan.down_delay_remaining_s
thermal.floor_pwm
thermal.owner
thermal.basis
thermal.cooling_levels
thermal.legacy_policy_pending
thermal.protections.policy
thermal.protections.intact
thermal.protections.governor_hijacked
thermal.protections.cpufreq_cooling
thermal.protections.trips
thermal.protections.warnings
control.temp_c
control.sensor_label
sensors[].value_c
sensors[].status
sensors[].valid
sensors[].source
sensors[].label
sensors[].error
sensors[].age_s
sensors[].title
events[].ts
events[].level
events[].code
events[].message
sample_mode
paths.uci_config
paths.state
paths.event_log
paths.lock_dir
'

SCHEMA="$("${BIN}" schema)"

for field in ${CONTRACT_FIELDS}; do
	printf '%s\n' "${SCHEMA}" | grep -qF "\"${field}\"" \
		|| fail "the contract no longer publishes ${field}"

	grep -qF "${field##*.}" "${VIEW}" \
		|| fail "the view never reads ${field} any more"
done

say 'checking the JSON document shape before anything was published'

JSON="$("${BIN}" status json)"
printf '%s\n' "${JSON}" | node -e '
const fs = require("fs");
const doc = JSON.parse(fs.readFileSync(0, "utf8"));
const assert = require("assert");

assert.strictEqual(doc.schema, 3, "schema version");
assert.ok(doc.controller && doc.settings && doc.fan && doc.thermal, "top level sections");
assert.strictEqual(doc.sensors.length, 5, "every sensor slot is present");
assert.strictEqual(doc.sample_mode, "cached", "default status must not resample");
assert.strictEqual(doc.controller.health, "stopped", "no snapshot yet");
assert.strictEqual(doc.controller.running, false);

const module = doc.sensors.find((entry) => entry.id === "module");
assert.ok(module, "module slot");
assert.strictEqual(module.value_c, null, "an unread sensor must be null, never 0");
assert.strictEqual(module.status, "missing");
assert.ok(module.error, "and it must say why");

assert.strictEqual(doc.fan.rpm, null, "no tachometer in this fixture");
assert.strictEqual(doc.fan.rpm_available, false);
assert.ok(doc.thermal.protections.trips !== undefined, "trip table is exposed");
' || fail 'the JSON contract does not match the documented shape'

say 'checking the legacy key=value output'

PLAIN="$("${BIN}" status)"
for key in enabled mode curve curve_data temp_source manual_pwm interval hysteresis \
	down_delay start_pwm start_boost_ms pwm pwm_value fan fan_feedback fan_rpm \
	cpu_temp cpu_label phy_temp phy_label wifi1_temp wifi1_label wifi2_temp \
	wifi2_label module_temp module_label control_temp control_sensor \
	thermal_floor thermal_state thermal_max_state thermal_owner result reason \
	requested_pwm target_pwm applied_pwm state_age; do
	printf '%s\n' "${PLAIN}" | grep -q "^${key}=" \
		|| fail "the legacy key ${key} disappeared from status output"
done

printf '%s\n' "${PLAIN}" | grep -q '^phy_temp=$' \
	|| fail 'an unread sensor must print as empty, not as 0'

say 'checking that a live sample and a cached sample agree on provenance'

LIVE="$("${BIN}" status live json)"
printf '%s\n' "${LIVE}" | node -e '
const fs = require("fs");
const doc = JSON.parse(fs.readFileSync(0, "utf8"));
const assert = require("assert");

assert.strictEqual(doc.sample_mode, "live");
const cpu = doc.sensors.find((entry) => entry.id === "cpu");
assert.strictEqual(cpu.value_c, 52);
assert.strictEqual(cpu.status, "ok");
' || fail 'live status does not report the simulated sensor'

say 'checking a control cycle writes the register and publishes state'

"${BIN}" apply > "${TMP}/apply.out" 2>&1 || fail "apply exited $?"
grep -q 'applied:' "${TMP}/apply.out" || fail "apply did not report its result"

# 52 C on the balanced curve is 58 percent (40:50 and 55:60 interpolated), and 58 * 255 / 100 truncates to 147.
pwm_now="$(tr -d '[:space:]' < "${TMP}/sys/class/hwmon/hwmon1/pwm1")"
[ "${pwm_now}" = "147" ] || fail "pwm1 holds ${pwm_now}, expected 147"

[ -f "${TMP}/var/run/h5000m-fancontrol.state" ] || fail 'no state snapshot was published'
grep -q '^applied_pwm=147$' "${TMP}/var/run/h5000m-fancontrol.state" \
	|| fail 'the snapshot does not record the applied output'
grep -q '^result=ok$' "${TMP}/var/run/h5000m-fancontrol.state" \
	|| fail 'the snapshot does not record a healthy cycle'

[ -f "${TMP}/var/run/h5000m-fancontrol.events" ] || fail 'no event journal was written'

say 'checking that a second controller defers instead of racing for the PWM node'

mkdir -p "${TMP}/var/lock/h5000m-fancontrol.lock"
printf '%s\n' "$$" > "${TMP}/var/lock/h5000m-fancontrol.lock/pid"
mkdir -p "${TMP}/proc/$$"
printf '%s\0%s\0' "h5000m-fancontrol" "daemon" > "${TMP}/proc/$$/cmdline"

"${BIN}" apply > "${TMP}/busy.out" 2>&1 || fail "apply should queue behind a live daemon, exited $?"
grep -q 'queued' "${TMP}/busy.out" || fail 'apply did not report that it queued the request'
[ -f "${TMP}/var/run/h5000m-fancontrol.wake" ] || fail 'no wake request was recorded'
[ "$(tr -d '[:space:]' < "${TMP}/sys/class/hwmon/hwmon1/pwm1")" = "147" ] \
	|| fail 'the queued apply wrote the register anyway'

rm -rf "${TMP}/var/lock/h5000m-fancontrol.lock" "${TMP}/proc"

say 'checking that an absent fan node is reported, not hidden'

rm -f "${TMP}/sys/class/hwmon/hwmon1/pwm1"
"${BIN}" status json | node -e '
const fs = require("fs");
const doc = JSON.parse(fs.readFileSync(0, "utf8"));
const assert = require("assert");

assert.strictEqual(doc.fan.pwm_path, null);
assert.ok(doc.controller.last_error, "a missing fan node must explain itself");
assert.strictEqual(doc.fan.reason, "pwm-missing");
' || fail 'a missing PWM node is not surfaced by status'

"${BIN}" apply > /dev/null 2>&1 && fail 'apply must exit non-zero when there is no PWM node'

say 'PASS: status contract, legacy output, control cycle and failure reporting'
