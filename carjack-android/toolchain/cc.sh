#!/bin/sh
# Clang from an Android source tree, against the NDK sysroot that tree's build generates.
T=${ANDROID_TREE:?set ANDROID_TREE to a built Android source tree}
C=$(ls -d "$T"/prebuilts/clang/host/linux-x86/clang-r* | sort | tail -1)
# Build scripts add a bare --target, which would drop the API level.
for arg; do
    shift
    case $arg in --target=*) ;; *) set -- "$@" "$arg" ;; esac
done
exec "$C/bin/clang" --target=aarch64-linux-android30 \
    --sysroot="$T/out/soong/ndk/sysroot" \
    -I"$(dirname "$0")/include" \
    -L"$C/runtimes_ndk_cxx/aarch64" -L"$C/runtimes_ndk_cxx" "$@"
