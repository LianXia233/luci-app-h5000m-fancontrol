#!/bin/sh
# Red-line guards for kernel thermal protection.
#
# These are static checks: they read the controller sources and the device tree
# patches, and never touch a real thermal zone or PWM node. The behavioural
# half of the same invariants lives in `cargo test -- protection` and in
# src/tests/regression.rs.

set -eu

# No CDPATH inheritance: `cd` must resolve relative paths from the filesystem.
CDPATH=''
ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
cd "${ROOT}"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT INT TERM

say() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Production code only: everything from the first `#[cfg(test)]` to the end of
# the file is test scaffolding, and tests are allowed to plant `user_space` in
# a sandbox file to prove the recovery path notices it.
prod() {
	awk '/^#\[cfg\(test\)\]/ { exit } { print }' "$1"
}

say 'checking that only the legacy recovery path writes a thermal policy'

# safety.rs is the only module that may write a governor file, and it must do so
# in exactly one place. This is a heuristic, not a proof; the behavioural half
# of the invariant is `cargo test -- protection` plus the regression suite.
prod src/src/safety.rs > "${TMP}/safety.rs"
writes="$(grep -c 'fs::write' "${TMP}/safety.rs" || true)"
[ "${writes}" -eq 1 ] \
	|| fail "safety.rs has ${writes} file writers; only the 2.x recovery path may write"
grep -q 'fn restore_legacy_policy' "${TMP}/safety.rs" \
	|| fail 'the single policy writer is not restore_legacy_policy'

for source in src/src/*.rs; do
	prod "${source}" > "${TMP}/prod.rs"
	if grep 'fs::write' "${TMP}/prod.rs" | grep -q 'user_space'; then
		fail "${source} writes the literal user_space into a file"
	fi
done

say 'checking that thermal policies are only addressed from safety.rs'

for source in src/src/*.rs; do
	case "${source}" in
	src/src/safety.rs) continue ;;
	esac
	prod "${source}" > "${TMP}/prod.rs"
	# Listing thermal zones to read a temperature is fine; reaching for the
	# governor file is not.
	if grep -qE 'join\("policy"\)|join\("available_policies"\)' "${TMP}/prod.rs"; then
		fail "${source} reaches into a thermal zone policy; only safety.rs may"
	fi
done

say 'checking that the recovery path demotes a recorded user_space governor'

prod src/src/safety.rs > "${TMP}/safety.rs"
grep -q 'Some("user_space")' "${TMP}/safety.rs" \
	|| fail 'safety.rs no longer demotes a recorded user_space governor'
grep -q '"step_wise"' "${TMP}/safety.rs" \
	|| fail 'safety.rs no longer has a kernel governor to fall back to'
grep -q 'fn inspect_kernel_protection' "${TMP}/safety.rs" \
	|| fail 'safety.rs no longer reports whether kernel protection is intact'

say 'checking that the device tree patches cannot remove CPU protection'

patches="$(find openwrt-patches -maxdepth 1 -name '*.patch' | sort)"
[ -n "${patches}" ] || fail 'no device tree patches found'

for patch in ${patches}; do
	# An additive patch cannot delete a trip point, a temperature or the CPU
	# cooling device: it only names fan cooling maps with /delete-node/.
	deleted="$(git apply --numstat "${patch}" | awk '{ print $2 }')"
	[ "${deleted}" = "0" ] || fail "${patch} removes ${deleted} lines; it must be additive"

	added="$(grep -E '^\+' "${patch}" | grep -v '^+++' | sed 's/^+//')"
	# Every /delete-node/ target must be a fan cooling map.
	targets="$(printf '%s\n' "${added}" | sed -n 's|.*/delete-node/ \{1,\}\([A-Za-z0-9_-]*\);.*|\1|p')"
	[ -n "${targets}" ] || fail "${patch} no longer deletes anything"
	for target in ${targets}; do
		case "${target}" in
		cpu-active-high | cpu-active-low | cpu-passive) ;;
		*) fail "${patch} deletes ${target}, which is not a fan cooling map" ;;
		esac
	done

	# The patched node has to be the fan cooling-map container, not a zone.
	printf '%s\n' "${added}" | grep -q 'thermal-zones/.*/cooling-maps' \
		|| fail "${patch} does not target a cooling-maps node"

	# Only device-tree code lines count: comments may say "hot and critical
	# trips stay", and the /delete-node/ names are checked separately.
	code="$(printf '%s\n' "${added}" \
		| sed 's|^[[:space:]]*||' \
		| grep -v '^/\*' | grep -v '^\*' | grep -v '/delete-node/' || true)"
	if printf '%s\n' "${code}" | grep -Eq 'temperature|critical|hot|cpufreq|trips'; then
		fail "${patch} assigns a thermal trip value"
	fi
done

say 'PASS: kernel thermal protection cannot be bypassed by the controller or the patches'
