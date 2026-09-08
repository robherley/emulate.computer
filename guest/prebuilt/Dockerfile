# Rebuild the checked-in RISC-V desktop binaries; context is guest/.
ARG ALPINE_BASE=alpine:3.23.5@sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40
ARG TAR_VERSION=1.35-r4

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

FROM ${ALPINE_BASE} AS base
SHELL ["/bin/sh", "-euc"]

# XLibre needs an absolute-pointer fix for its kdrive evdev driver.
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
RUN apk add --no-cache build-base libx11-dev
ADD https://codeload.github.com/ozkl/doomgeneric/tar.gz/${DOOMGENERIC_REV} /src/doomgeneric.tar.gz
COPY patches/doom/ /src/patches/
RUN cd /src; \
    echo "${DOOMGENERIC_SHA256}  doomgeneric.tar.gz" | sha256sum -c -; \
    mkdir doomgeneric; tar xf doomgeneric.tar.gz -C doomgeneric --strip-components=1; \
    cd doomgeneric; \
    for fix in /src/patches/*.patch; do patch -p1 < "$fix"; done; \
    make -C doomgeneric -j4 CC=cc CFLAGS='-O2 -std=gnu17 -DNORMALUNIX -DLINUX -D_DEFAULT_SOURCE'; \
    install -Dm755 doomgeneric/doomgeneric /out/usr/bin/doomgeneric; \
    strip /out/usr/bin/doomgeneric; \
    install -Dm644 LICENSE /out/usr/share/licenses/doomgeneric/LICENSE

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

FROM base AS bundle
ARG TAR_VERSION
RUN apk add --no-cache "tar=${TAR_VERSION}"
COPY --from=xserver /out/Xfbdev /bundle/usr/bin/Xfbdev
COPY --from=xserver /src/server/COPYING /bundle/usr/share/licenses/xlibre/COPYING
COPY --from=window-manager /out/usr/ /bundle/usr/
COPY --from=games /out/usr/ /bundle/usr/
COPY --from=doom /out/usr/ /bundle/usr/
COPY --from=desktop-apps /out/usr/ /bundle/usr/
RUN mkdir -p /out; \
    tar --numeric-owner --owner=0 --group=0 --sort=name --mtime=@0 --format=gnu \
      -cf /out/desktop.tar -C /bundle usr; \
    gzip -9 -n /out/desktop.tar

FROM scratch AS out
COPY --from=bundle /out/desktop.tar.gz /desktop.tar.gz
