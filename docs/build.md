# 构建、交叉编译与打包

## 目标平台

Hiveton H5000M 使用 MT7987A（ARMv8‑A 64 位），OpenWrt 目标为
`mediatek/filogic`，musl ABI，因此 Rust 目标三元组是 **`aarch64-unknown-linux-musl`**。

`src/Makefile` 里按 `$(ARCH)` 映射三元组，并覆盖 `aarch64`、`armv7`、`x86_64`、`i686`
四种 musl 目标；未知架构直接报错，而不是悄悄产出一个跑不了的二进制。

## 本地构建控制器

```sh
# 主机调试构建（快速跑测试）
cd src && cargo test --release

# 路由器目标构建：静态 musl，无需交叉 gcc
cd src && rustup target add aarch64-unknown-linux-musl
cd src && cargo build --release --target aarch64-unknown-linux-musl
file target/aarch64-unknown-linux-musl/release/h5000m-fancontrol
# 期望：ELF 64-bit ARM aarch64, statically linked, stripped
```

链接器在 `src/.cargo/config.toml` 里固定为工具链自带的 `rust-lld`，musl 目标默认
`crt-static`，所以整条链路不需要外部交叉 gcc 或目标 sysroot，也不会误用宿主机 gcc。

## 打成 OpenWrt 软件包

`luci.mk` 检测到 `src/Makefile` 后会：把包标记为架构相关（不再是 `all`）、在
`PKG_BUILD_DIR` 内执行 `make -C src clean compile`，再以 `DESTDIR=$(PKG_INSTALL_DIR)`
执行 `make -C src install`，把二进制安装到 `/usr/sbin/h5000m-fancontrol`（0755）。

集成进 OpenWrt 源码树：

```sh
git clone https://github.com/LianXia233/luci-app-h5000m-fancontrol.git \
  package/luci-app-h5000m-fancontrol
make menuconfig     # LuCI -> Applications -> luci-app-h5000m-fancontrol
make package/luci-app-h5000m-fancontrol/compile V=s
```

构建主机的要求：可用的 `cargo`（建议 rustup 安装）+ 对应 musl target；
crate 无依赖且带 `Cargo.lock`，`src/Makefile` 使用 `--offline`，SDK 构建不联网取源码。

`src/Makefile` 在构建后还会用 `file(1)` 校验产物的 ELF 机器类型，主机二进制混进来
会直接失败（错误信息里带期望的三元组）。

## 发布构建（GitHub Actions + 官方 SDK）

`scripts/build-release.sh` 下载官方 OpenWrt SNAPSHOT `mediatek/filogic` SDK，
校验 `sha256sums`，装好 `luci-base`，加上 `aarch64-unknown-linux-musl` target 后编译本包，然后：

1. 从产物里取 `luci-app-h5000m-fancontrol` 与 `luci-i18n-h5000m-fancontrol-zh-cn`
   的 `.apk`（或 `.ipk`）。
2. **打开软件包验证其中的控制器确实是静态 aarch64 ELF**（APKv2 是三段拼接的
   gzipped tar，读取数据段必须 `tar --ignore-zeros`，否则只能看到签名段）。
3. 用 SDK 构建密钥 `apk adbsign` 签名，再用**只有公钥**的 keys 目录 `apk verify`。
4. 写 `BUILD-INFO.txt`（SDK 版本、源码提交、SDK 压缩包校验值），最后经临时文件
   生成 `SHA256SUMS`，避免自校验与自引用。

`public-key.pem` 与 `openwrt-sdk-build.pem` 是发布物；`private-key.pem` 只存在于
SDK 环境里，**绝不进入仓库、软件包或发布产物**。CI 有一个 job 专门检查这一点
（`tests/test-packaging.sh`）。

## 版本

`Makefile` 的 `PKG_VERSION` 与 `src/Cargo.toml` 的 `version` 同步；
GitHub Release 标签为 `v<version>`，安装包版本为 `<version>-r<PKG_RELEASE>`。
三处一致性由发布工作流在打标签时校验。

## 设备树补丁

`openwrt-patches/h5000m-userspace-fan-control.patch` 只删除 `cpu-thermal` 的三个
风扇 cooling-map（`cpu-active-high`、`cpu-active-low`、`cpu-passive`），CPU 降频、
`hot`、`critical` 节点保留。CI 会检查该补丁没有删除任何触发点。详见
[safety.md](safety.md)。
