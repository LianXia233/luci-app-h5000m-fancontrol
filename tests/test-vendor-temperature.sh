#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
# Source only function definitions; never run the host fan apply command.
sed '/^case /,$d' "$ROOT/root/usr/sbin/h5000m-fancontrol" > "$TMP/functions"
export H5000M_FAN_SENSOR_SYSFS_ROOT="$TMP/sys"
export H5000M_FAN_RADIO_SYSFS_ROOT="$TMP/net"
mkdir -p "$TMP/net/ra0" "$TMP/net/rai0"
. "$TMP/functions"
TEMP_SOURCE=max
find_module_temp() { :; }
iwpriv() {
    echo "$1" >> "$TMP/calls"
    case "${CASE:-valid}:$1" in
        valid:ra0) echo 'CurrentTemperature = 42';;
        valid:rai0) echo 'CurrentTemperature = 57';;
        invalid:*) echo 'CurrentTemperature = 900';;
        malformed:*) echo 'CurrentTemperature = 42 junk';;
        fail:*) return 1;;
    esac
}
timeout() { shift; "$@"; }
read_sensors
test "$WIFI1_TEMP" = 42; test "$WIFI2_TEMP" = 57
test "$CONTROL_TEMP" = 57; test "$CONTROL_SENSOR" = 'MTK radio rai0'
CASE=invalid; read_sensors; test -z "$WIFI1_TEMP$WIFI2_TEMP$CONTROL_TEMP"
CASE=malformed; read_sensors; test -z "$WIFI1_TEMP$WIFI2_TEMP"
CASE=fail; read_sensors; test -z "$WIFI1_TEMP$WIFI2_TEMP"
# Native sensors must win; no driver query when both are present.
mkdir -p "$TMP/sys/class/hwmon/hwmon1" "$TMP/sys/class/hwmon/hwmon2"
printf mt7996 > "$TMP/sys/class/hwmon/hwmon1/name"
printf 61000 > "$TMP/sys/class/hwmon/hwmon1/temp1_input"
printf wifi > "$TMP/sys/class/hwmon/hwmon2/name"
printf 62000 > "$TMP/sys/class/hwmon/hwmon2/temp1_input"
before=$(wc -l < "$TMP/calls"); CASE=valid; read_sensors
test "$WIFI1_TEMP" = 61; test "$WIFI2_TEMP" = 62
test "$(wc -l < "$TMP/calls")" = "$before"
# Timeout or unavailable helper must leave a missing temperature, not hang.
timeout() { return 124; }
test -z "$(read_vendor_radio_temp ra0 || true)"
unset -f iwpriv
test -z "$(read_vendor_radio_temp ra0 || true)"
echo 'PASS: bounded vendor fallback, invalid/failing helpers, native priority and max policy'
