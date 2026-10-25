#!/bin/sh
# Regression test: sensor discovery against a simulated sysfs tree.
# Covers native thermal-zone/hwmon priority, vendor iwpriv fallback, max vs
# cpu control selection, unit conversion, invalid readings and the modem
# temperature caches. Runs entirely against H5000M_TEST_BIN with env
# overrides - never touches real hardware.
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
mkdir -p "$SYS/class/thermal" "$SYS/class/hwmon" "$NET" "$RUN" "$TMP/tmpdir" "$TMP/path"

write() {
	mkdir -p "$(dirname "$1")"
	printf '%b' "$2" > "$1"
}

# A fake uci so config options can be exercised on the test host.
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

cat > "$TMP/path/iwpriv" << 'EOF'
#!/bin/sh
# test double for the MTK vendor ioctl helper
case "${CASE:-}:${1}" in
	valid:ra0) echo 'ra0       CurrentTemperature = 42' ;;
	valid:rai0) echo 'rai0      CurrentTemperature = 57' ;;
	invalid:*) echo 'CurrentTemperature = 900' ;;
	malformed:*) echo 'CurrentTemperature = 42 junk' ;;
	fail:*) exit 1 ;;
	*) exit 1 ;;
esac
EOF
chmod +x "$TMP/path/iwpriv"

export H5000M_FAN_SENSOR_SYSFS_ROOT="$SYS"
export H5000M_FAN_PWM_SYSFS_ROOT="$SYS"
export H5000M_FAN_RADIO_SYSFS_ROOT="$NET"
export H5000M_FAN_DT_ROOT="$TMP/dt"
export H5000M_FAN_STATE_DIR="$RUN"
export H5000M_FAN_LOCK_DIR="$TMP/lock/h5000m-fancontrol.lock"
export H5000M_FAN_MT5700M_TEMP_CACHE="$TMP/none/temperature"
export H5000M_FAN_LEGACY_CACHE_DIR="$TMP/empty-tmp"
export PATH="$TMP/path:$PATH"

fan_hwmon() {
	write "$SYS/class/hwmon/hwmon0/name" 'pwmfan\n'
	write "$SYS/class/hwmon/hwmon0/pwm1" '0\n'
}

# --- 1. native sensors win over the vendor fallback, max policy -----------
fan_hwmon
write "$SYS/class/thermal/thermal_zone0/type" 'soc_thermal\n'
write "$SYS/class/thermal/thermal_zone0/temp" '62000\n'
write "$SYS/class/hwmon/hwmon1/name" 'mt7996\n'
write "$SYS/class/hwmon/hwmon1/temp1_input" '67000\n'
write "$SYS/class/hwmon/hwmon2/name" 'mt753x-mdio\n'
write "$SYS/class/hwmon/hwmon2/temp1_input" '51000\n'
mkdir -p "$NET/ra0" "$NET/rai0"
out="$(CASE=valid "$BIN" status)"
echo "$out" | grep -qx 'cpu_temp=62'
echo "$out" | grep -qx 'wifi1_temp=67'
echo "$out" | grep -qx 'wifi2_temp=57'  # no native wifi2 -> vendor fallback fills it
echo "$out" | grep -qx 'phy_temp=51'
echo "$out" | grep -qx 'control_temp=67'
echo "$out" | grep -qx 'control_sensor=mt7996'

# --- 2. vendor fallback fills only missing slots --------------------------
# (no wifi hwmon chips at all -> iwpriv answers for both radios)
rm -rf "$SYS/class/hwmon/hwmon1" "$SYS/class/hwmon/hwmon2"
out="$(CASE=valid "$BIN" status)"
echo "$out" | grep -qx 'wifi1_temp=42'
echo "$out" | grep -qx 'wifi1_label=MTK radio ra0'
echo "$out" | grep -qx 'wifi2_temp=57'
echo "$out" | grep -qx 'control_temp=62'   # cpu 62 beats radio 57
echo "$out" | grep -qx 'control_sensor=soc_thermal'

# --- 3. invalid / failing vendor output is unknown, never zero ------------
out="$(CASE=invalid "$BIN" status)"
echo "$out" | grep -qx 'wifi1_temp=' && echo "$out" | grep -qx 'wifi2_temp='
out="$(CASE=malformed "$BIN" status)"
echo "$out" | grep -qx 'wifi1_temp='
out="$(CASE=fail "$BIN" status)"
echo "$out" | grep -qx 'wifi1_temp='

# --- 4. unit conversion and bounds ----------------------------------------
write "$SYS/class/thermal/thermal_zone0/temp" '45600\n'
out="$("$BIN" status)"
echo "$out" | grep -qx 'cpu_temp=45'
write "$SYS/class/thermal/thermal_zone0/temp" '999\n'   # stays raw -> out of range
out="$("$BIN" status)"
echo "$out" | grep -qx 'cpu_temp='
write "$SYS/class/thermal/thermal_zone0/temp" '-41000\n'
out="$("$BIN" status)"
echo "$out" | grep -qx 'cpu_temp='
write "$SYS/class/thermal/thermal_zone0/temp" 'garbage\n'
out="$("$BIN" status)"
echo "$out" | grep -qx 'cpu_temp='

# --- 5. cpu-only control source -------------------------------------------
write "$SYS/class/thermal/thermal_zone0/temp" '62000\n'
write "$SYS/class/hwmon/hwmon1/name" 'mt7996\n'
write "$SYS/class/hwmon/hwmon1/temp1_input" '88000\n'
set_cfg "temp_source='cpu'"
out="$("$BIN" status)"
echo "$out" | grep -qx 'control_temp=62'
set_cfg "temp_source='max'"
out="$("$BIN" status)"
echo "$out" | grep -qx 'control_temp=88'

# --- 6. modem manager cache with freshness --------------------------------
now="$(date +%s)"
printf 'temperature=43.6\ntemperature_sensor=modem2\nupdated=%s\n' "$now" > "$TMP/modem-temperature"
export H5000M_FAN_MT5700M_TEMP_CACHE="$TMP/modem-temperature"
out="$("$BIN" status)"
echo "$out" | grep -qx 'module_temp=44'
stale=$((now - 61))
printf 'temperature=43.6\nupdated=%s\n' "$stale" > "$TMP/modem-temperature"
out="$("$BIN" status)"
echo "$out" | grep -qx 'module_temp='
unset H5000M_FAN_MT5700M_TEMP_CACHE

# --- 7. legacy QModem cache fallback --------------------------------------
printf '{"key": "temperature", "value": "41"}\n' > "$TMP/tmpdir/cache_base_info_0"
export H5000M_FAN_LEGACY_CACHE_DIR="$TMP/tmpdir"
out="$("$BIN" status)"
echo "$out" | grep -qx 'module_temp=41'
unset H5000M_FAN_LEGACY_CACHE_DIR

# --- 8. status --json is well-formed and never reports 0 for missing ------
out="$("$BIN" status --json)"
echo "$out" | grep -q '"controller"'
echo "$out" | grep -q '"sensors"'
echo "$out" | grep -q '"fan"'
echo "$out" | grep -q '"safety"'
echo "$out" | grep -q '"phy":{"temp_c":null'

echo 'PASS: sensor discovery, vendor fallback, unit conversion, max/cpu policy, modem caches'
