#!/bin/sh
# Packaging guards: the things that must be true for the router to get a real
# cross-compiled controller instead of a host binary, a stale shell script or a
# leaked signing key.

set -eu

# No CDPATH inheritance: `cd` must resolve relative paths from the filesystem.
CDPATH=''
ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
cd "${ROOT}"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

say 'the shell controller is gone and the built binary is not committed'

[ ! -e root/usr/sbin/h5000m-fancontrol ] \
	|| fail 'root/usr/sbin/h5000m-fancontrol must be produced by the build, not shipped in git'
git ls-files --error-unmatch root/usr/sbin/h5000m-fancontrol >/dev/null 2>&1 \
	&& fail 'the controller binary is tracked by git'
grep -qx 'root/usr/sbin/h5000m-fancontrol' .gitignore \
	|| fail 'the built controller is not ignored, so it can be committed by accident'

say 'the package keeps its integration points'

[ -f root/usr/sbin/h5000m-fancontrol-status ] \
	|| fail 'the status wrapper disappeared'
grep -q 'h5000m-fancontrol status "\$@"' root/usr/sbin/h5000m-fancontrol-status \
	|| fail 'the status wrapper no longer forwards its arguments'
for path in \
	root/etc/init.d/h5000m-fancontrol \
	root/etc/config/h5000m_fancontrol \
	root/etc/uci-defaults/91-h5000m-fancontrol-v3 \
	root/usr/share/luci/menu.d/luci-app-h5000m-fancontrol.json \
	root/usr/share/rpcd/acl.d/luci-app-h5000m-fancontrol.json; do
	[ -f "${path}" ] || fail "${path} is missing"
done

grep -q '/etc/config/h5000m_fancontrol' Makefile \
	|| fail 'the UCI config must stay a conffile so upgrades keep user settings'
grep -q 'LUCI_PKGARCH:=all' Makefile \
	&& fail 'a package carrying a native binary must not be marked arch all'

say 'everything the system executes is installed executable'

# A 0644 uci-defaults script is silently skipped on first boot and a 0644 test
# is unrunnable in CI, so the mode is part of the integration contract.
for path in \
	root/etc/init.d/h5000m-fancontrol \
	root/etc/uci-defaults/91-h5000m-fancontrol-v3 \
	root/usr/sbin/h5000m-fancontrol-status \
	scripts/build-release.sh \
	tests/test-status-contract.sh \
	tests/test-uci-defaults.sh \
	tests/test-packaging.sh \
	tests/test-kernel-safety.sh; do
	mode="$(git ls-files -s "${path}" | awk '{ print $1 }')"
	[ "${mode}" = "100755" ] \
		|| fail "${path} is ${mode:-not tracked}; it is executed directly and needs mode 100755"
done

say 'procd supervises the daemon and saving does not restart it'

grep -q 'USE_PROCD=1' root/etc/init.d/h5000m-fancontrol \
	|| fail 'the service must use procd'
grep -q 'h5000m-fancontrol daemon' root/etc/init.d/h5000m-fancontrol \
	|| fail 'procd must run the controller in the foreground'
grep -q 'procd_set_param respawn' root/etc/init.d/h5000m-fancontrol \
	|| fail 'the service must be respawned'
grep -q 'procd_send_signal' root/etc/init.d/h5000m-fancontrol \
	|| fail 'reload must signal the daemon instead of restarting it'
! grep -qE '^\s*(stop|restart)\s*$' root/etc/init.d/h5000m-fancontrol \
	|| fail 'reload_service must not stop and start the controller'

say 'rpcd permissions stay split between reading and writing'

node -e '
const fs = require("fs");
const acl = JSON.parse(fs.readFileSync("root/usr/share/rpcd/acl.d/luci-app-h5000m-fancontrol.json", "utf8"));
const assert = require("assert");
const grant = acl["luci-app-h5000m-fancontrol"];

assert.deepStrictEqual(Object.keys(grant.read.file), ["/usr/sbin/h5000m-fancontrol-status"],
	"read access must be limited to the status wrapper");
assert.deepStrictEqual(grant.read.file["/usr/sbin/h5000m-fancontrol-status"], ["exec"]);
assert.ok(grant.write.file["/usr/sbin/h5000m-fancontrol"], "applying needs the controller");
assert.ok(grant.write.file["/etc/init.d/h5000m-fancontrol"], "the service control needs the init script");
assert.ok(grant.read.uci.includes("h5000m_fancontrol"));
assert.ok(grant.write.uci.includes("h5000m_fancontrol"));
assert.ok(!JSON.stringify(acl).includes("sys"), "the ACL must not grant shell execution");
' || fail 'the rpcd ACL is no longer correct'

say 'the LuCI menu keeps its place under System'

node -e '
const fs = require("fs");
const assert = require("assert");
const menu = JSON.parse(fs.readFileSync("root/usr/share/luci/menu.d/luci-app-h5000m-fancontrol.json", "utf8"));
const key = "admin/system/h5000m_fancontrol";
const entry = menu[key];

assert.ok(entry, "the page must stay under admin/system");
assert.strictEqual(entry.action.type, "view");
assert.strictEqual(entry.action.path, "h5000m/fancontrol");
assert.ok(entry.depends.acl.includes("luci-app-h5000m-fancontrol"), "menu must depend on the ACL");
assert.deepStrictEqual(Object.keys(entry.depends.uci), ["h5000m_fancontrol"]);
' || fail 'the LuCI menu entry changed unexpectedly'

say 'no private key or secret material is tracked'

if git ls-files | grep -Eiq '(^|/)(private-key|signing-key|id_rsa|id_ed25519|.*\.pem$|.*\.key$)'; then
	fail 'a private key or certificate file is tracked by git'
fi

# The pattern below spells the markers it looks for, so this file has to be
# excluded or the guard would always match itself.
git grep -I -n -E '(BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|-----BEGIN CERTIFICATE)' -- . \
	':(exclude)tests/test-packaging.sh' \
	&& fail 'private key material appears in the repository'

# Static inspection only: never execute the release script here, it downloads
# the OpenWrt SDK and builds packages.
grep -q 'private-key.pem' scripts/build-release.sh \
	|| fail 'the release script no longer signs packages'
grep -q -- '--sign-key' scripts/build-release.sh \
	|| fail 'APK signing is missing'
grep -q 'apk .*verify' scripts/build-release.sh \
	|| fail 'signed APKs are not verified again'
grep -q 'SHA256SUMS' scripts/build-release.sh \
	|| fail 'release artefacts are not checksummed'
! grep -qE 'public-key\.pem.*dist-release|dist-release.*private' scripts/build-release.sh \
	|| fail 'only the public key may be published'

say 'the crate is wired for a reproducible offline cross build'

[ -f src/Cargo.toml ] || fail 'the Rust crate is missing'
[ -f src/Cargo.lock ] || fail 'Cargo.lock must be committed for --offline builds'
[ -f src/.cargo/config.toml ] || fail 'the musl linker configuration is missing'
grep -q 'publish = false' src/Cargo.toml || fail 'the crate must not be publishable'
# The dependency block must stay empty; any entry here needs a review.
entries="$(awk '/^\[dependencies\]$/{flag=1;next}/^\[/{flag=0}flag' src/Cargo.toml | grep -cvE '^\s*(#|$)' || true)"
[ "${entries}" = "0" ] || fail 'the controller gained an external dependency'
grep -q 'rust-lld' src/.cargo/config.toml || fail 'no linker is pinned for the musl targets'
grep -q 'aarch64-unknown-linux-musl' src/Makefile || fail 'the SDK wrapper does not build the router target'
grep -q 'statically linked\|FILE_MACHINE_' src/Makefile \
	|| fail 'the build does not verify the binary architecture'

say 'PASS: packaging guards'
