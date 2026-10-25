include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-h5000m-fancontrol
PKG_VERSION:=3.1.0
PKG_RELEASE:=1
PKG_LICENSE:=Apache-2.0
PKG_LICENSE_FILES:=LICENSE

LUCI_TITLE:=H5000M fan control
LUCI_DESCRIPTION:=Temperature aware PWM fan control for the Hiveton H5000M with \
a native Rust controller, kernel safety floors and a tabbed LuCI UI.
LUCI_DEPENDS:=+luci-base +kmod-hwmon-pwmfan
# src/Makefile exists, so luci.mk keeps this package architecture specific and
# builds the Rust controller for the selected target instead of shipping `all`.

define Package/luci-app-h5000m-fancontrol/conffiles
/etc/config/h5000m_fancontrol
endef

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature
