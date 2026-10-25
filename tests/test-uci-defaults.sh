#!/bin/sh
# Migration test for root/etc/uci-defaults/91-h5000m-fancontrol-v3.
#
# OpenWrt is not available here, so `uci` is replaced by a stub that keeps the
# config in a plain file with the same get/set/delete/commit semantics the real
# one uses for the options this script touches. The init script call is stubbed
# too, because a test host has no /etc/init.d.

set -eu

# No CDPATH inheritance: `cd` must resolve relative paths from the filesystem.
CDPATH=''
ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
SCRIPT="${ROOT}/root/etc/uci-defaults/91-h5000m-fancontrol-v3"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT INT TERM

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

[ -f "${SCRIPT}" ] || fail 'the migration script is missing'
[ -f "${ROOT}/root/etc/uci-defaults/91-h5000m-fancontrol-v2" ] \
	&& fail 'the superseded v2 script must not ship alongside v3'

mkdir -p "${TMP}/bin" "${TMP}/etc/config" "${TMP}/state"

cat > "${TMP}/bin/uci" <<'EOF'
#!/bin/sh
# Minimal uci stand-in: get/set/delete/commit over one delta file. The real
# command is usually called as `uci -q get ...`, so the quiet flag is stripped
# before dispatch.
db="${UCI_STUB_DB}"
[ -f "${db}" ] || : > "${db}"
[ "${1:-}" = "-q" ] && shift

case "$1 ${2:-}" in
	'get'*)
		opt="${2#h5000m_fancontrol.settings.}"
		[ "$2" = "${opt}" ] && exit 1
		line="$(grep -m1 "^${opt}=" "${db}" || true)"
		[ -n "${line}" ] || exit 1
		printf '%s\n' "${line#*=}"
		;;
	'set'*)
		arg="$2"
		opt="${arg%%=*}"; value="${arg#*=}"
		opt="${opt#h5000m_fancontrol.settings.}"
		grep -v "^${opt}=" "${db}" > "${db}.tmp" || true
		printf '%s=%s\n' "${opt}" "${value}" >> "${db}.tmp"
		mv "${db}.tmp" "${db}"
		;;
	'delete'*)
		opt="${2#h5000m_fancontrol.settings.}"
		grep -v "^${opt}=" "${db}" > "${db}.tmp" || true
		mv "${db}.tmp" "${db}"
		;;
	'commit'*)
		printf 'committed\n' >> "${UCI_STUB_LOG}"
		;;
	*)
		:
		;;
esac
exit 0
EOF

cat > "${TMP}/bin/h5000m-fancontrol-service" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "${SERVICE_STUB_LOG}"
EOF

chmod +x "${TMP}/bin/uci" "${TMP}/bin/h5000m-fancontrol-service"

export PATH="${TMP}/bin:${PATH}"
export UCI_STUB_DB="${TMP}/state/db"
export UCI_STUB_LOG="${TMP}/state/uci-log"
export SERVICE_STUB_LOG="${TMP}/state/service-log"
: > "${UCI_STUB_DB}"
: > "${UCI_STUB_LOG}"
: > "${SERVICE_STUB_LOG}"

# /etc/init.d/<service> is stubbed through a wrapper directory so the script can
# run unmodified on a host without OpenWrt.
mkdir -p "${TMP}/etc/init.d"
printf '#!/bin/sh\nexec h5000m-fancontrol-service "$@"\n' > "${TMP}/etc/init.d/h5000m-fancontrol"
chmod +x "${TMP}/etc/init.d/h5000m-fancontrol"

run_migration() {
	# Only the init.d path differs on a test host; the uci calls are real.
	sed "s|/etc/init.d/h5000m-fancontrol|${TMP}/etc/init.d/h5000m-fancontrol|g" \
		"${SCRIPT}" > "${TMP}/run.sh"
	sh "${TMP}/run.sh"
}

say 'migrating a 1.x configuration that used the old mode name'

cat > "${UCI_STUB_DB}" <<'EOF'
mode=off
min_pwm=60
max_pwm=200
low_temp=40
high_temp=80
manual_pwm=99
EOF

run_migration

grep -q '^mode=kernel$' "${UCI_STUB_DB}" || fail 'the legacy off mode was not mapped to kernel'
grep -q '^schema_version=3$' "${UCI_STUB_DB}" || fail 'the schema was not stamped'
grep -q '^manual_pwm=99$' "${UCI_STUB_DB}" || fail 'an operator value must survive the migration'
for removed in min_pwm max_pwm low_temp high_temp; do
	grep -q "^${removed}=" "${UCI_STUB_DB}" && fail "${removed} was not removed"
done
for option in temp_source interval hysteresis down_delay start_pwm \
	start_boost_ms fail_pwm curve_silent curve_balanced curve_performance curve_custom; do
	grep -q "^${option}=" "${UCI_STUB_DB}" || fail "${option} was not filled with its default"
done
grep -q '^fail_pwm=255$' "${UCI_STUB_DB}" || fail 'fail_pwm lost its shipped default'
grep -q '^curve_custom=20:0,40:50,55:60,70:75,85:90,95:100$' "${UCI_STUB_DB}" \
	|| fail 'curve_custom lost its shipped default'

# Options that existed since 1.x are deliberately left alone; the controller
# substitutes its own defaults for absent keys, so writing them here would only
# risk overwriting an operator's choice.
grep -q '^enabled=' "${UCI_STUB_DB}" && fail 'enabled must not be forced by the migration'
grep -q '^curve=' "${UCI_STUB_DB}" && fail 'curve must not be forced by the migration'

say 'running the migration again changes nothing'

cp "${UCI_STUB_DB}" "${TMP}/state/first"
run_migration
diff -u "${TMP}/state/first" "${UCI_STUB_DB}" > "${TMP}/state/second.diff" \
	|| fail 'the migration is not idempotent'

say 'a 2.x configuration keeps every value the operator set'

cat > "${UCI_STUB_DB}" <<'EOF'
schema_version=2
enabled=1
mode=auto
curve=silent
temp_source=cpu
manual_pwm=120
interval=7
hysteresis=4
down_delay=90
start_pwm=200
start_boost_ms=500
fail_pwm=240
curve_custom=20:0,50:70,90:100
EOF

run_migration

grep -q '^curve=silent$' "${UCI_STUB_DB}" || fail 'the chosen profile changed'
grep -q '^interval=7$' "${UCI_STUB_DB}" || fail 'the sampling interval changed'
grep -q '^fail_pwm=240$' "${UCI_STUB_DB}" || fail 'the failsafe output changed'
grep -q '^curve_custom=20:0,50:70,90:100$' "${UCI_STUB_DB}" || fail 'the custom curve changed'
grep -q '^schema_version=3$' "${UCI_STUB_DB}" || fail 'the schema was not upgraded'

say 'the service is enabled and restarted so the new controller takes over'

grep -q '^enable$' "${SERVICE_STUB_LOG}" || fail 'the migration did not enable the service'
grep -q '^restart$' "${SERVICE_STUB_LOG}" || fail 'the migration did not restart the service'
[ -s "${UCI_STUB_LOG}" ] || fail 'the migrated values were never committed to UCI'

say 'PASS: uci-defaults migration maps legacy values, is idempotent and keeps user data'
