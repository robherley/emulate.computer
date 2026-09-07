# Pinned riscv64 guest artifacts collected by the final `out` stage.
ARG ALPINE_BASE=alpine:3.23.5@sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40

ARG KERNEL_VERSION=6.18.44-r0
ARG MODULE_DIR=6.18.44-0-lts

ARG BUSYBOX_STATIC_VERSION=1.37.0-r30
ARG CPIO_VERSION=2.15-r0
ARG DTC_VERSION=1.7.2-r0
ARG TAR_VERSION=1.35-r4
ARG OPENSBI_VERSION=1.7-r0

ARG PYTHON3_VERSION=3.12.14-r0
ARG BASH_VERSION_PIN=5.3.3-r1
ARG QUICKJS_VERSION=0.20250426-r0
ARG CURL_VERSION=8.22.0-r0
ARG CA_CERTIFICATES_VERSION=20260611-r0

# XLibre Xfbdev, built without the accelerated graphics stack.
ARG XLIBRE_VERSION=25.1.9
ARG XLIBRE_SHA256=be30a5ede7975995b196717af190664eb07aa43e3608eaf9c582386941715f8e
ARG FLUXBOX_VERSION=1.3.7
ARG FLUXBOX_SHA256=d47a58edba857f3c36bf8430bbd17834693ad0e6aa431d3507039f022af7aee8
ARG FOX_VERSION=1.6.59
ARG FOX_SHA256=48f33d2dd5371c2d48f6518297f0ef5bbf3fcd37719e99f815dc6fc6e0f928ae
ARG ACE_VERSION=1.4
ARG ACE_SHA256=1fee3b0533921a42873c0623f33d873a067b1eec58f2c70c01449146e79a4cce
ARG ACE_APORTS_REV=3b19afb4254a25d2584c5710c60023c4c5f97157
ARG XTRIS_VERSION=1.15
ARG XTRIS_SHA256=5791b30795382a18536201c95c686f62ade6975fc4e63c651bff36226c933d68
ARG DOOMGENERIC_REV=dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284
ARG DOOMGENERIC_SHA256=1bd3f7f26220494159a38d71f2847ec81b58d6bbd7c7c8d81b08993018001148
ARG FREEDOOM_VERSION=0.13.0
ARG FREEDOOM_SHA256=3f9b264f3e3ce503b4fb7f6bdcb1f419d93c7b546f4df3e874dd878db9688f59
ARG XTERM_VERSION=403-r0
ARG DILLO_VERSION=3.2.0-r0
ARG XCALC_VERSION=1.1.2-r0
ARG XCLOCK_VERSION=1.1.1-r0
ARG XEYES_VERSION=1.3.1-r0
ARG FONT_MISC_MISC_VERSION=1.1.3-r1
ARG FONT_CURSOR_MISC_VERSION=1.0.4-r1
ARG XSETROOT_VERSION=1.1.3-r1
ARG XPROP_VERSION=1.2.8-r0
ARG FONTCONFIG_VERSION=2.17.1-r0
ARG XKBCOMP_VERSION=1.5.0-r0
ARG XKEYBOARD_CONFIG_VERSION=2.46-r0
ARG LIBXFONT2_VERSION=2.0.7-r0
ARG PIXMAN_VERSION=0.46.4-r0
ARG LIBXKBFILE_VERSION=1.1.3-r0
ARG LIBXAU_VERSION=1.0.12-r0
ARG LIBBSD_VERSION=0.12.2-r0
ARG LIBXRANDR_VERSION=1.5.4-r1
ARG LIBSTDCXX_VERSION=15.2.0-r2

# Fixed timestamps for reproducible archives and font caches.
ARG INITRAMFS_STAMP=197001010000.00
ARG ROOTFS_STAMP=202506300000.00

FROM ${ALPINE_BASE} AS base
SHELL ["/bin/sh", "-euc"]

# Kernel and required modules, without linux-firmware.
FROM base AS kernel
ARG KERNEL_VERSION
ARG MODULE_DIR
RUN mkdir -p /work /out/modules; \
    cd /work; \
    apk fetch --no-cache --quiet linux-lts; \
    test -f "linux-lts-${KERNEL_VERSION}.apk" || { \
      echo "error: apk fetch resolved $(echo linux-lts-*.apk), not linux-lts-${KERNEL_VERSION}.apk" >&2; \
      echo "bump KERNEL_VERSION and MODULE_DIR in guest/Dockerfile" >&2; \
      exit 2; \
    }; \
    tar xzf "linux-lts-${KERNEL_VERSION}.apk" boot/vmlinuz-lts; \
    gunzip -c boot/vmlinuz-lts > /out/Image; \
    for module in \
      drivers/virtio/virtio \
      drivers/virtio/virtio_ring \
      drivers/virtio/virtio_mmio \
      drivers/char/virtio_console \
      drivers/block/virtio_blk \
      drivers/char/hw_random/virtio-rng \
      net/core/failover \
      drivers/net/net_failover \
      drivers/net/virtio_net \
      net/packet/af_packet \
      drivers/virtio/virtio_input \
      drivers/input/evdev \
      lib/crc/crc16 \
      fs/mbcache \
      fs/jbd2/jbd2 \
      fs/ext4/ext4; do \
        path="lib/modules/${MODULE_DIR}/kernel/${module}.ko.gz"; \
        tar xzf "linux-lts-${KERNEL_VERSION}.apk" "$path"; \
        gunzip -c "$path" > "/out/modules/${module##*/}.ko"; \
    done; \
    rm -rf /work

# PID 1 initramfs.
FROM base AS initramfs
ARG MODULE_DIR
ARG BUSYBOX_STATIC_VERSION
ARG CPIO_VERSION
ARG INITRAMFS_STAMP
RUN apk add --no-cache "busybox-static=${BUSYBOX_STATIC_VERSION}" "cpio=${CPIO_VERSION}"
COPY --from=kernel /out/modules /modules
COPY overlays/initramfs/init /src/init
COPY overlays/initramfs/etc/inittab /src/inittab
COPY overlays/initramfs/etc/profile /src/profile
COPY overlays/initramfs/etc/motd /src/motd
COPY overlays/initramfs/usr/share/udhcpc/default.script /src/udhcpc-default.script
RUN umask 022; \
    mkdir -p /irfs; \
    cd /irfs; \
    mkdir -m 0755 -p dev proc sys etc bin sbin usr/bin usr/sbin \
      usr/share/udhcpc "lib/modules/${MODULE_DIR}"; \
    mkdir -m 0700 -p root; \
    mkdir -m 1777 -p tmp; \
    mknod dev/console c 5 1;  chmod 0600 dev/console; \
    mknod dev/null    c 1 3;  chmod 0666 dev/null; \
    mknod dev/zero    c 1 5;  chmod 0666 dev/zero; \
    mknod dev/ttyS0   c 4 64; chmod 0620 dev/ttyS0; \
    install -m 0755 /bin/busybox.static bin/busybox; \
    ln -s /bin/busybox bin/sh; \
    install -m 0755 /src/init init; \
    install -m 0644 /src/inittab etc/inittab; \
    install -m 0644 /src/profile etc/profile; \
    install -m 0644 /src/motd etc/motd; \
    install -m 0755 /src/udhcpc-default.script usr/share/udhcpc/default.script; \
    for module in /modules/*.ko; do \
      install -m 0644 "$module" "lib/modules/${MODULE_DIR}/${module##*/}"; \
    done; \
    chown -R 0:0 .; \
    TZ=UTC find . -exec touch -h -t "${INITRAMFS_STAMP}" {} +; \
    mkdir -p /out; \
    find . -print0 | LC_ALL=C sort -z \
      | cpio --reproducible --null --quiet -o -H newc > /out/initramfs.cpio; \
    gzip -9 -n /out/initramfs.cpio; \
    ls -l /out/initramfs.cpio.gz

# Device trees.
FROM base AS dtb
ARG DTC_VERSION
RUN apk add --no-cache "dtc=${DTC_VERSION}"
COPY dts/virt.dts /src/virt.dts
COPY dts/build.sh /src/build.sh
COPY --from=initramfs /out/initramfs.cpio.gz /src/initramfs.cpio.gz
RUN sh /src/build.sh /src/virt.dts /src/initramfs.cpio.gz /out

# OpenSBI firmware.
FROM base AS firmware
ARG OPENSBI_VERSION
RUN apk add --no-cache "opensbi=${OPENSBI_VERSION}"; \
    mkdir -p /out; \
    cp /usr/share/opensbi/generic/firmware/fw_dynamic.bin /out/fw.bin; \
    chmod 0644 /out/fw.bin

# XLibre still needs the absolute-pointer fix for its kdrive evdev driver.
FROM base AS xserver
ARG XLIBRE_VERSION
ARG XLIBRE_SHA256
RUN apk add --no-cache \
      build-base pkgconf patch meson ninja python3 linux-headers \
      xorgproto xtrans libx11-dev libbsd-dev \
      libxfont2-dev pixman-dev libxkbfile-dev libxau-dev openssl-dev zlib-dev
ADD https://github.com/X11Libre/xserver/archive/refs/tags/xlibre-xserver-${XLIBRE_VERSION}.tar.gz \
    /src/xlibre.tar.gz
COPY patches/xserver/ /src/patches/
RUN cd /src; \
    echo "${XLIBRE_SHA256}  xlibre.tar.gz" | sha256sum -c -; \
    mkdir server; \
    tar xf xlibre.tar.gz -C server --strip-components=1; \
    cd server; \
    for patch in /src/patches/*.patch; do patch -p1 < "$patch"; done; \
    meson setup build --prefix=/usr --sysconfdir=/etc --localstatedir=/var \
      --buildtype=minsize --wrap-mode=nofallback \
      -Dxfbdev=true -Dxorg=false -Dxephyr=false -Dxnest=false \
      -Dxvfb=false -Dxwin=false -Dxquartz=false \
      -Dkdrive_evdev=true -Dkdrive_kbd=false -Dkdrive_mouse=false \
      -Dglamor=false -Dglx=false -Dglx_dri=false -Ddrm=false \
      -Ddri1=false -Ddri2=false -Ddri3=false -Dpciaccess=false \
      -Dudev=false -Dudev_kms=false -Dseatd_libseat=false \
      -Dsystemd_logind=false -Dsystemd_notify=false \
      -Dxdmcp=false -Dxdm-auth-1=false -Dsha1=libcrypto \
      -Dtests=false -Ddocs=false -Ddevel-docs=false \
      -Dfontrootdir=/usr/share/fonts \
      -Ddefault_font_path=/usr/share/fonts/misc,/usr/share/fonts/100dpi,/usr/share/fonts/75dpi \
      -Dxkb_output_dir=/var/lib/xkb -Dxkb_bin_dir=/usr/bin -Dxkb_dir=/usr/share/X11/xkb; \
    ninja -C build hw/kdrive/fbdev/Xfbdev; \
    mkdir -p /out; \
    install -m 0755 build/hw/kdrive/fbdev/Xfbdev /out/Xfbdev; \
    strip /out/Xfbdev; \
    ls -l /out/Xfbdev; \
    if ldd /out/Xfbdev | grep -Ei "libgl|llvm|gallium|mesa|libdrm"; then \
      echo "error: Xfbdev links against graphics-stack libraries" >&2; exit 2; \
    fi; \
    ldd /out/Xfbdev

# Fluxbox supplies window management, the menu, and toolbar in one process.
FROM base AS window-manager
ARG FLUXBOX_VERSION
ARG FLUXBOX_SHA256
RUN apk add --no-cache build-base pkgconf automake libxpm-dev libx11-dev libxext-dev libxrandr-dev
ADD https://distfiles.alpinelinux.org/distfiles/v3.23/fluxbox-${FLUXBOX_VERSION}.tar.bz2 /src/fluxbox.tar.bz2
COPY patches/fluxbox/ /src/patches/
RUN cd /src; \
    echo "${FLUXBOX_SHA256}  fluxbox.tar.bz2" | sha256sum -c -; \
    mkdir fluxbox; tar xf fluxbox.tar.bz2 -C fluxbox --strip-components=1; cd fluxbox; \
    for fix in /src/patches/*.patch; do patch -p1 < "$fix"; done; \
    cp /usr/share/automake-*/config.guess /usr/share/automake-*/config.sub build-aux/; \
    CXXFLAGS="-Os -std=gnu++11" ./configure --prefix=/usr --sysconfdir=/etc \
      --disable-nls --disable-xmb --disable-slit --disable-systray \
      --disable-imlib2 --disable-xft --disable-freetype2 --disable-xrender \
      --disable-xinerama --disable-fribidi \
      --with-init=/etc/fluxbox/init --with-keys=/etc/fluxbox/keys \
      --with-menu=/etc/fluxbox/menu --with-windowmenu=/etc/fluxbox/windowmenu \
      --with-style=/etc/fluxbox/style --with-apps=/etc/fluxbox/apps \
      --with-overlay=/etc/fluxbox/overlay; \
    make src/defaults.hh src/defaults.cc; \
    make -j4 fluxbox; \
    install -Dm755 fluxbox /out/usr/bin/fluxbox; \
    strip /out/usr/bin/fluxbox; \
    install -Dm644 COPYING /out/usr/share/licenses/fluxbox/COPYING

# Small X11 games; assets and help are compiled into the Ace binaries.
FROM base AS games
ARG ACE_VERSION
ARG ACE_SHA256
ARG ACE_APORTS_REV
ARG XTRIS_VERSION
ARG XTRIS_SHA256
RUN apk add --no-cache build-base autoconf automake libtool libpng-dev libx11-dev libxpm-dev zlib-dev
ADD https://www.delorie.com/store/ace/ace-${ACE_VERSION}.tar.gz /src/ace.tar.gz
ADD https://raw.githubusercontent.com/alpinelinux/aports/${ACE_APORTS_REV}/testing/ace-of-penguins/ace.patch /src/ace.patch
ADD https://raw.githubusercontent.com/alpinelinux/aports/${ACE_APORTS_REV}/testing/ace-of-penguins/fix-font.patch /src/fix-font.patch
ADD https://raw.githubusercontent.com/alpinelinux/aports/${ACE_APORTS_REV}/testing/ace-of-penguins/gcc14.patch /src/gcc14.patch
COPY patches/games/ /src/patches/
RUN cd /src; \
    echo "${ACE_SHA256}  ace.tar.gz" | sha256sum -c -; \
    echo 'df7034518f8220d5f0e3c5ea18c1c4e0d98044f541e11f5771fb15cf0b0053ff  ace.patch' | sha256sum -c -; \
    echo '25bca72d6fa4779651604e6e43f22a2ff905421341fdeb4d9eedf70779c57794  fix-font.patch' | sha256sum -c -; \
    echo '05ef31a10757b564ab72b3193452af21fa2ffc02004642b30df771eb1b226123  gcc14.patch' | sha256sum -c -; \
    mkdir ace; tar xf ace.tar.gz -C ace --strip-components=1; cd ace; \
    for fix in ace fix-font gcc14; do patch -p1 < "/src/$fix.patch"; done; \
    for fix in /src/patches/*.patch; do patch -p1 < "$fix"; done; \
    autoreconf -fi; \
    CFLAGS='-Os -std=gnu17' ./configure --prefix=/usr --disable-static; \
    make -j4 AM_LDFLAGS='-lX11 -lpng -lz -lm'; \
    mkdir -p /out/usr/bin /out/usr/lib /out/usr/share/licenses/ace-of-penguins; \
    for game in solitaire minesweeper; do \
      install -m755 "games/.libs/$game" "/out/usr/bin/ace-$game"; \
      strip "/out/usr/bin/ace-$game"; \
    done; \
    cp -a lib/.libs/libcards.so* /out/usr/lib/; \
    strip /out/usr/lib/libcards.so.1.0.0; \
    install -m644 COPYING /out/usr/share/licenses/ace-of-penguins/COPYING
ADD https://archive.debian.org/debian/pool/main/x/xtris/xtris_${XTRIS_VERSION}.orig.tar.gz /src/xtris.tar.gz
RUN cd /src; \
    echo "${XTRIS_SHA256}  xtris.tar.gz" | sha256sum -c -; \
    tar xf xtris.tar.gz; cd xtris; \
    make xtris BINDIR=/usr/bin CFLAGS='-Os -std=gnu17'; \
    install -m755 xtris /out/usr/bin/; \
    strip /out/usr/bin/xtris; \
    mkdir -p /out/usr/share/licenses/xtris; \
    install -m644 COPYING /out/usr/share/licenses/xtris/COPYING

FROM base AS doom
ARG DOOMGENERIC_REV
ARG DOOMGENERIC_SHA256
ARG FREEDOOM_VERSION
ARG FREEDOOM_SHA256
RUN apk add --no-cache build-base libx11-dev unzip
ADD https://codeload.github.com/ozkl/doomgeneric/tar.gz/${DOOMGENERIC_REV} /src/doomgeneric.tar.gz
ADD https://github.com/freedoom/freedoom/releases/download/v${FREEDOOM_VERSION}/freedoom-${FREEDOOM_VERSION}.zip /src/freedoom.zip
COPY patches/doom/ /src/patches/
RUN cd /src; \
    echo "${DOOMGENERIC_SHA256}  doomgeneric.tar.gz" | sha256sum -c -; \
    echo "${FREEDOOM_SHA256}  freedoom.zip" | sha256sum -c -; \
    mkdir doomgeneric; tar xf doomgeneric.tar.gz -C doomgeneric --strip-components=1; \
    cd doomgeneric; \
    for fix in /src/patches/*.patch; do patch -p1 < "$fix"; done; \
    make -C doomgeneric -j4 CC=cc CFLAGS='-O2 -std=gnu17 -DNORMALUNIX -DLINUX -D_DEFAULT_SOURCE'; \
    install -Dm755 doomgeneric/doomgeneric /out/usr/bin/doomgeneric; \
    strip /out/usr/bin/doomgeneric; \
    install -Dm644 LICENSE /out/usr/share/licenses/doomgeneric/LICENSE; \
    cd /src; unzip -q freedoom.zip; \
    install -Dm644 "freedoom-${FREEDOOM_VERSION}/freedoom1.wad" /out/usr/share/games/doom/freedoom1.wad; \
    mkdir -p /out/usr/share/licenses/freedoom; \
    cp "freedoom-${FREEDOOM_VERSION}/COPYING.txt" "freedoom-${FREEDOOM_VERSION}/CREDITS.txt" \
      "freedoom-${FREEDOOM_VERSION}/CREDITS-MUSIC.txt" /out/usr/share/licenses/freedoom/

# Shared toolkit for the text editor and file browser, without OpenGL.
FROM base AS desktop-apps
ARG FOX_VERSION
ARG FOX_SHA256
RUN apk add --no-cache build-base autoconf automake libtool libxft-dev libxext-dev libpng-dev zlib-dev bzip2-dev libjpeg-turbo-dev
ADD https://distfiles.alpinelinux.org/distfiles/v3.23/fox-${FOX_VERSION}.tar.gz /src/fox.tar.gz
RUN cd /src; \
    echo "${FOX_SHA256}  fox.tar.gz" | sha256sum -c -; \
    mkdir fox; tar xf fox.tar.gz -C fox --strip-components=1; cd fox; \
    autoreconf -fi; \
    ./configure --prefix=/usr --enable-release --disable-static --with-opengl=no --with-xft=yes \
      --disable-tiff; \
    make -j4 SUBDIRS='utils include src adie pathfinder'; \
    mkdir -p /out/usr/bin /out/usr/lib /out/usr/share/licenses/fox /out/usr/share/licenses/adie /out/usr/share/licenses/pathfinder; \
    cp -a src/.libs/libFOX-1.6.so* /out/usr/lib/; \
    strip /out/usr/lib/libFOX-1.6.so.0.0.59; \
    install -m755 adie/.libs/adie pathfinder/.libs/PathFinder /out/usr/bin/; \
    strip /out/usr/bin/adie /out/usr/bin/PathFinder; \
    install -m644 adie/Adie.stx /out/usr/bin/; \
    install -m644 LICENSE LICENSE_ADDENDUM /out/usr/share/licenses/fox/; \
    install -m644 adie/LICENSE /out/usr/share/licenses/adie/; \
    install -m644 pathfinder/LICENSE /out/usr/share/licenses/pathfinder/

# Alpine root filesystem content.
FROM base AS rootfs
ARG MODULE_DIR
ARG PYTHON3_VERSION
ARG BASH_VERSION_PIN
ARG QUICKJS_VERSION
ARG CURL_VERSION
ARG CA_CERTIFICATES_VERSION
ARG XTERM_VERSION
ARG DILLO_VERSION
ARG XCALC_VERSION
ARG XCLOCK_VERSION
ARG XEYES_VERSION
ARG FONT_MISC_MISC_VERSION
ARG FONT_CURSOR_MISC_VERSION
ARG XSETROOT_VERSION
ARG XPROP_VERSION
ARG FONTCONFIG_VERSION
ARG XKBCOMP_VERSION
ARG XKEYBOARD_CONFIG_VERSION
ARG LIBXFONT2_VERSION
ARG PIXMAN_VERSION
ARG LIBXKBFILE_VERSION
ARG LIBXAU_VERSION
ARG LIBBSD_VERSION
ARG LIBXRANDR_VERSION
ARG LIBSTDCXX_VERSION
ARG ROOTFS_STAMP

RUN apk add --no-cache \
      "python3=${PYTHON3_VERSION}" \
      "bash=${BASH_VERSION_PIN}" \
      "quickjs=${QUICKJS_VERSION}" \
      "curl=${CURL_VERSION}" \
      "ca-certificates=${CA_CERTIFICATES_VERSION}"

# X clients and Xfbdev runtime dependencies.
RUN apk add --no-cache \
      "libxfont2=${LIBXFONT2_VERSION}" \
      "pixman=${PIXMAN_VERSION}" \
      "libxkbfile=${LIBXKBFILE_VERSION}" \
      "libxau=${LIBXAU_VERSION}" \
      "libbsd=${LIBBSD_VERSION}" \
      "font-misc-misc=${FONT_MISC_MISC_VERSION}" \
      "font-cursor-misc=${FONT_CURSOR_MISC_VERSION}" \
      "xterm=${XTERM_VERSION}" \
      "dillo=${DILLO_VERSION}" \
      "xcalc=${XCALC_VERSION}" \
      "xclock=${XCLOCK_VERSION}" \
      "xeyes=${XEYES_VERSION}" \
      "xsetroot=${XSETROOT_VERSION}" \
      "xprop=${XPROP_VERSION}" \
      "xkbcomp=${XKBCOMP_VERSION}" \
      "xkeyboard-config=${XKEYBOARD_CONFIG_VERSION}" \
      "fontconfig=${FONTCONFIG_VERSION}" \
      "libxrandr=${LIBXRANDR_VERSION}" \
      "libstdc++=${LIBSTDCXX_VERSION}"

COPY --from=window-manager /out/usr/ /usr/
COPY --from=games /out/usr/ /usr/
COPY --from=doom /out/usr/ /usr/
COPY --from=desktop-apps /out/usr/ /usr/

# Keep the rootfs free of Mesa/LLVM.
RUN for binary in /usr/bin/fluxbox /usr/bin/dillo /usr/bin/xterm \
      /usr/bin/xcalc /usr/bin/xclock /usr/bin/xeyes /usr/bin/adie /usr/bin/PathFinder /usr/bin/ace-* /usr/bin/xtris /usr/bin/doomgeneric; do \
      if ldd "$binary" | grep -Ei "lib(GL|EGL|GLX|gbm|drm|LLVM)\.so|gallium|mesa"; then \
        echo "error: $binary links against graphics-stack libraries" >&2; \
        exit 2; \
      fi; \
    done; \
    if apk info 2>/dev/null | grep -Ei "^(mesa|llvm)"; then \
      echo "error: a Mesa or LLVM package reached the root filesystem" >&2; \
      exit 2; \
    fi

# Enable and pre-cache the bitmap-only font set.
RUN rm -f /etc/fonts/conf.d/70-no-bitmaps-except-emoji.conf; \
    ln -s /usr/share/fontconfig/conf.avail/70-yes-bitmaps.conf \
      /etc/fonts/conf.d/70-yes-bitmaps.conf; \
    TZ=UTC find /usr/share/fonts /usr/share/fontconfig /etc/fonts \
      -exec touch -h -t "${ROOTFS_STAMP}" {} +; \
    fc-cache -f; \
    test "$(fc-list | wc -l)" -gt 0 || { \
      echo "error: fontconfig indexed no fonts; Xft clients will not start" >&2; exit 2; \
    }; \
    TZ=UTC find /usr/share/fonts /usr/share/fontconfig /etc/fonts \
      -exec touch -h -t "${ROOTFS_STAMP}" {} +; \
    if fc-cache -v 2>&1 | grep -q "caching, new cache contents"; then \
      echo "error: the fontconfig cache does not survive its own stamp" >&2; \
      exit 2; \
    fi; \
    printf '%s\n' "${ROOTFS_STAMP}" > /etc/emulate-rootfs-stamp; \
    fc-list | wc -l; \
    fc-match sans
COPY --from=xserver --chmod=0755 /out/Xfbdev /usr/bin/Xfbdev
COPY --chmod=0755 overlays/rootfs/usr/local/bin/emuctl /usr/local/bin/emuctl
COPY --chmod=0755 overlays/rootfs/usr/local/bin/doom /usr/local/bin/doom

COPY --chmod=0644 overlays/rootfs/etc/fluxbox/ /etc/fluxbox/
# Avoid first-run setup copying the default init over the explicit -rc file.
RUN mkdir -p /root/.fluxbox

COPY --chmod=0644 overlays/rootfs/usr/share/emulate/ /usr/share/emulate/

# Dillo defaults.
RUN printf '%s\n' \
      '' \
      'home="file:///usr/share/emulate/index.html"' \
      'start_page="file:///usr/share/emulate/index.html"' \
      'geometry=900x500+120+160' \
      'font_serif="Misc Fixed"' \
      'font_sans_serif="Misc Fixed"' \
      'font_cursive="Misc Fixed"' \
      'font_fantasy="Misc Fixed"' \
      'font_monospace="Misc Fixed"' \
      'font_min_size=8' \
      'font_max_size=24' \
      'small_icons=YES' \
      >> /etc/dillo/dillorc

# Console and login configuration.
COPY --chmod=0644 overlays/rootfs/etc/inittab /etc/inittab
COPY --chmod=0644 overlays/rootfs/etc/profile /etc/profile
COPY --chmod=0644 overlays/rootfs/etc/motd /etc/motd

# Root is the interactive runtime account; use bash for both serial and xterm.
RUN sed -i 's|^\(root:[^:]*:0:0:[^:]*:/root:\)[^:]*$|\1/bin/bash|' /etc/passwd; \
    grep -q '^root:x:0:0:root:/root:/bin/bash$' /etc/passwd
COPY overlays/rootfs/root/ /root/

# Mount points and fstab.
RUN mkdir -p /dev /proc /sys /root /tmp; \
    chmod 0755 /root; \
    chmod 1777 /tmp; \
    printf '/dev/vda / ext4 rw 0 1\ntmpfs /tmp tmpfs rw,mode=1777,nosuid,nodev 0 0\n' \
      > /etc/fstab; \
    chmod 0644 /etc/fstab

# Deterministic rootfs archive for mke2fs.
FROM base AS rootfs-tar
ARG TAR_VERSION
RUN apk add --no-cache "tar=${TAR_VERSION}"
COPY --from=rootfs / /rootfs
RUN mkdir -p /out; \
    rm -f /rootfs/.dockerenv; \
    tar --numeric-owner --sort=name --format=gnu \
      -cf /out/rootfs.tar -C /rootfs .

# Exported artifacts.
FROM scratch AS out
COPY --from=firmware /out/fw.bin /fw.bin
COPY --from=kernel /out/Image /Image
COPY --from=initramfs /out/initramfs.cpio.gz /initramfs.cpio.gz
COPY --from=dtb /out/virt.dtb /virt.dtb
COPY --from=dtb /out/virt-rootfs.dtb /virt-rootfs.dtb
COPY --from=rootfs-tar /out/rootfs.tar /rootfs.tar
COPY --from=dtb /out/virt-desktop.dtb /virt-desktop.dtb
