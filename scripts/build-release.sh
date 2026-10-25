#!/usr/bin/env bash
# Build the H5000M packages with the official OpenWrt SNAPSHOT SDK.
#
# The Rust controller is compiled by the package build itself (src/Makefile), so
# this script only has to provide a Rust toolchain with the router's musl
# target and then verify what came out of the SDK.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_dir="${RUNNER_TEMP:-/tmp}/h5000m-fan-sdk"
output_dir="${repo_dir}/dist-release"
base_url="https://downloads.openwrt.org/snapshots/targets/mediatek/filogic"

mkdir -p "${work_dir}" "${output_dir}"
find "${output_dir}" -mindepth 1 -maxdepth 1 -delete
cd "${work_dir}"
curl -fsSLO "${base_url}/sha256sums"
archive="$(awk '/openwrt-sdk-.*Linux-x86_64\.tar\.zst$/ { print $2; exit }' sha256sums | sed 's/^\*//')"
test -n "${archive}"
curl -fL --retry 5 "${base_url}/${archive}" -o "${archive}"
grep "[ *]${archive}$" sha256sums | sha256sum -c -
tar --zstd -xf "${archive}"
sdk_dir="$(find "${work_dir}" -maxdepth 1 -type d -name 'openwrt-sdk-*' | head -n 1)"
test -n "${sdk_dir}"

cd "${sdk_dir}"
./scripts/feeds update -a
./scripts/feeds install luci-base

# The Rust controller cross-compiles inside the SDK package build. Make sure the
# toolchain and the musl target for the router architecture are present; the
# crate has no dependencies and builds offline from Cargo.lock.
command -v cargo >/dev/null || { echo 'cargo not found: install a Rust toolchain first' >&2; exit 1; }
rustup target add aarch64-unknown-linux-musl
cargo --version
rustc --version

perl -0pi -e 's/(config ALL\n\s+bool "Select all userspace packages by default"\n\s+default )y/${1}n/' Config.in
perl -0pi -e 's/(config TARGET_MULTI_PROFILE\n\s+bool\n\s+default )y/${1}n/; s/(config TARGET_ALL_PROFILES\n\s+bool\n\s+default )y/${1}n/; s/(config TARGET_DEVICE_mediatek_filogic_DEVICE_[^\n]+\n\s+bool\n\s+default )y/${1}n/g' Config-build.in
sed -i 's/^[[:space:]]*default m$/\tdefault n/' Config-build.in

mkdir -p package/h5000m-custom
rsync -a --exclude '.git/' --exclude '.github/' --exclude 'dist/' --exclude 'dist-release/' \
	--exclude 'src/target/' --exclude 'backend/target/' \
	"${repo_dir}/" package/h5000m-custom/luci-app-h5000m-fancontrol/
cat > .config <<'EOF'
CONFIG_TARGET_mediatek=y
CONFIG_TARGET_mediatek_filogic=y
# CONFIG_ALL is not set
# CONFIG_ALL_KMODS is not set
# CONFIG_ALL_NONSHARED is not set
CONFIG_PACKAGE_luci-app-h5000m-fancontrol=m
CONFIG_LUCI_LANG_zh_Hans=y
EOF
make defconfig
make package/h5000m-custom/luci-app-h5000m-fancontrol/compile -j"$(nproc)" V=s

find bin -type f \( -name 'luci-app-h5000m-fancontrol-*.apk' -o -name 'luci-app-h5000m-fancontrol_*.ipk' -o -name 'luci-i18n-h5000m-fancontrol-zh-cn-*.apk' -o -name 'luci-i18n-h5000m-fancontrol-zh-cn_*.ipk' \) -exec cp -f {} "${output_dir}/" \;
test "$(find "${output_dir}" -type f \( -name '*.apk' -o -name '*.ipk' \) | wc -l)" -ge 2

# Prove the packaged controller really is an aarch64 static binary and not a
# host build that was copied in.
app_apk="$(find "${output_dir}" -type f -name 'luci-app-h5000m-fancontrol-*.apk' | head -n 1)"
app_ipk="$(find "${output_dir}" -type f -name 'luci-app-h5000m-fancontrol_*.ipk' | head -n 1)"
if [ -n "${app_apk}" ]; then
  # An APKv2 file is three concatenated gzipped tar streams (signature, control,
  # data); without --ignore-zeros tar stops at the first end-of-archive marker
  # and only ever sees the signature segment.
  binary_info="$(tar -tzif "${app_apk}" | grep -m1 'usr/sbin/h5000m-fancontrol$' || true)"
  test -n "${binary_info}" || { echo 'controller binary missing from apk' >&2; exit 1; }
  tar -xzif "${app_apk}" -O "${binary_info}" > "${output_dir}/.controller.check"
  file "${output_dir}/.controller.check" | grep -Eq 'ELF 64-bit.*aarch64.*statically linked' || {
    echo 'packaged controller is not a statically linked aarch64 ELF' >&2
    file "${output_dir}/.controller.check" >&2
    exit 1
  }
  rm -f "${output_dir}/.controller.check"
elif [ -n "${app_ipk}" ]; then
  # IPK is an ar archive of two tar streams; the control/data pair.
  rm -rf "${output_dir}/.ipk"
  mkdir -p "${output_dir}/.ipk"
  (cd "${output_dir}/.ipk" && ar x "${app_ipk}" && tar xzf data.tar.gz)
  test -f "${output_dir}/.ipk/usr/sbin/h5000m-fancontrol" \
	|| { echo 'controller binary missing from ipk' >&2; exit 1; }
  file "${output_dir}/.ipk/usr/sbin/h5000m-fancontrol" | grep -Eq 'ELF 64-bit.*aarch64.*statically linked' \
	|| { echo 'packaged controller is not a statically linked aarch64 ELF' >&2; exit 1; }
  rm -rf "${output_dir}/.ipk"
else
  echo 'no app package was produced' >&2
  exit 1
fi

if find "${output_dir}" -name '*.apk' | grep -q .; then
  cp public-key.pem "${output_dir}/openwrt-sdk-build.pem"
  mkdir -p "${work_dir}/verification-keys"
  cp public-key.pem "${work_dir}/verification-keys/build.pem"
  for package in "${output_dir}"/*.apk; do
    # Only our freshly built package is accepted unsigned at the signing step.
    staging_dir/host/bin/apk adbsign --allow-untrusted --sign-key private-key.pem "$package"
    staging_dir/host/bin/apk --keys-dir "${work_dir}/verification-keys" verify "$package"
  done
fi

printf 'SDK=snapshot\nSOURCE_COMMIT=%s\nTARGET=mediatek/filogic\n' "$(git -C "$repo_dir" rev-parse HEAD)" > "${output_dir}/BUILD-INFO.txt"
grep "[ *]${archive}$" "${work_dir}/sha256sums" >> "${output_dir}/BUILD-INFO.txt"
# Checksums last, staged through a temp file, so SHA256SUMS never lists itself
# and no pipeline reads and writes the same file.
(cd "${output_dir}" && find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.tmp' -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS.tmp && mv SHA256SUMS.tmp SHA256SUMS)
