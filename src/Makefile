# Rust build wrapper invoked by the OpenWrt / LuCI package build.
#
# luci.mk detects this Makefile and (a) marks the package as
# architecture-specific instead of `all`, (b) runs `make -C src clean
# compile` inside PKG_BUILD_DIR and `make -C src install` with
# DESTDIR=$(PKG_INSTALL_DIR). The controller itself is a dependency-free
# Rust crate that links fully static against musl, so building it needs
# nothing but a Rust toolchain (rustup) with the right target installed.

RUST_TARGET_aarch64 := aarch64-unknown-linux-musl
RUST_TARGET_arm     := armv7-unknown-linux-musleabihf
RUST_TARGET_x86_64  := x86_64-unknown-linux-musl
RUST_TARGET_i386    := i686-unknown-linux-musl

RUST_TARGET := $(RUST_TARGET_$(ARCH))

ifeq ($(RUST_TARGET),)
$(error h5000m-fancontrol: unsupported build architecture '$(ARCH)'; the H5000M firmware is aarch64 / mediatek-filogic)
endif

CARGO ?= $(shell command -v cargo 2>/dev/null)
ifeq ($(CARGO),)
$(error h5000m-fancontrol: cargo not found in PATH - install a Rust toolchain, e.g. via https://rustup.rs)
endif

# luci.mk invokes this Makefile as a plain sub-make with DESTDIR and the
# toolchain variables only - the OpenWrt install helpers are not defined in
# that context. Fall back to the canonical OpenWrt definitions when missing,
# otherwise $(INSTALL_DIR) expands to nothing and install tries to execute
# the DESTDIR path as a command (error 127).
ifeq ($(strip $(INSTALL_DIR)),)
INSTALL_DIR := install -d
endif
ifeq ($(strip $(INSTALL_BIN)),)
INSTALL_BIN := install -m0755
endif

# Expected ELF machine description per target, used to prove the binary was
# really cross-compiled for the router instead of picking up a host build.
FILE_MACHINE_aarch64-unknown-linux-musl     := aarch64
FILE_MACHINE_armv7-unknown-linux-musleabihf := ARM
FILE_MACHINE_x86_64-unknown-linux-musl      := x86-64
FILE_MACHINE_i686-unknown-linux-musl        := 80386

# cargo --target places the binary under target/<triple>/release/.
BINARY := target/$(RUST_TARGET)/release/h5000m-fancontrol

# The crate has no dependencies and ships Cargo.lock, so an offline build is
# fully reproducible and never touches the network.
CARGO_FLAGS := --release --offline --target $(RUST_TARGET)

all: compile

compile:
	$(CARGO) build $(CARGO_FLAGS)
	@test -x $(BINARY) || { \
		echo 'ERROR: $(BINARY) missing after cargo build' >&2; \
		exit 1; \
	}
	@if command -v file >/dev/null 2>&1; then \
		file $(BINARY) | grep -q '$(FILE_MACHINE_$(RUST_TARGET))' || { \
			echo 'ERROR: $(BINARY) is not a $(RUST_TARGET) ELF binary' >&2; \
			exit 1; \
		}; \
	else \
		echo 'note: file(1) unavailable, skipping ELF architecture check' >&2; \
	fi

clean:
	rm -rf target

install: compile
	$(INSTALL_DIR) $(DESTDIR)/usr/sbin
	$(INSTALL_BIN) $(BINARY) $(DESTDIR)/usr/sbin/h5000m-fancontrol

.PHONY: all compile clean install
