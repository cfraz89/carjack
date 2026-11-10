# The NDK's own toolchain, for the Gradle build's cargo-ndk: CMake-built crates (libopus) are given
# only CMAKE_SYSTEM_NAME=Android, which CMake can't place without an NDK path and ABI.
set(ANDROID_ABI arm64-v8a)
set(ANDROID_PLATFORM 30)
include($ENV{ANDROID_NDK_HOME}/build/cmake/android.toolchain.cmake)
