#!/usr/bin/env bash
#
# Install the RTL-SDR Blog V4 driver (the rtl-sdr-blog fork is REQUIRED for the
# V4; the stock Debian/osmocom librtlsdr does not tune the V4 correctly).
# Run with sudo on Raspberry Pi OS / Debian (aarch64).
#
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }

echo "==> Removing any distro rtl-sdr that would conflict with the blog driver"
apt-get remove -y rtl-sdr librtlsdr-dev librtlsdr0 2>/dev/null || true

echo "==> Build dependencies"
apt-get update
apt-get install -y git cmake build-essential libusb-1.0-0-dev pkg-config

echo "==> Building rtl-sdr-blog fork"
SRC=/usr/local/src/rtl-sdr-blog
rm -rf "$SRC"
git clone --depth 1 https://github.com/rtlsdrblog/rtl-sdr-blog "$SRC"
cmake -S "$SRC" -B "$SRC/build" -DINSTALL_UDEV_RULES=ON -DDETACH_KERNEL_DRIVER=ON
cmake --build "$SRC/build" -j"$(nproc)"
cmake --install "$SRC/build"
ldconfig

echo "==> Blacklisting the DVB-T kernel module so it doesn't grab the dongle"
cat > /etc/modprobe.d/blacklist-rtlsdr.conf <<'EOF'
blacklist dvb_usb_rtl28xxu
blacklist rtl2832
blacklist rtl2830
EOF

cat <<EOF

Done. Replug the RTL-SDR (or reboot to apply the blacklist), then verify:
  rtl_test -t
You should see "RTL-SDR Blog V4 Detected". If the DVB module still grabs it:
  sudo rmmod dvb_usb_rtl28xxu 2>/dev/null || true
EOF
