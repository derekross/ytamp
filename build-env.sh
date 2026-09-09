# Source this before cargo commands on hosts without system libasound2-dev.
# Deb-extracted sysroot (Shanty pattern) — no root required.
export PKG_CONFIG_PATH="$HOME/sysroot/usr/lib/x86_64-linux-gnu/pkgconfig:${PKG_CONFIG_PATH:-}"
export RUSTFLAGS="-L $HOME/sysroot/usr/lib/x86_64-linux-gnu ${RUSTFLAGS:-}"
