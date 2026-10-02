# CMake's own Android platform wants a standalone NDK; build as plain Linux with our clang.
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR aarch64)
set(CMAKE_C_COMPILER ${CMAKE_CURRENT_LIST_DIR}/cc.sh)
set(CMAKE_CXX_COMPILER ${CMAKE_CURRENT_LIST_DIR}/cc.sh)
set(CMAKE_ASM_COMPILER ${CMAKE_CURRENT_LIST_DIR}/cc.sh)
# The tree's sysroot has no crt objects for executables.
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)
