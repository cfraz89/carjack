plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.compose) apply false
}

allprojects {
    layout.buildDirectory = rootDir.resolve("../target/gradle/$name")
}

// The native core, cross-built with the NDK, for both apps' jniLibs.
val nativeLibs: File = layout.buildDirectory.dir("cargo-jni").get().asFile
extra["nativeLibs"] = nativeLibs
tasks.register<Exec>("cargoNdk") {
    val repo = rootDir.parentFile
    val toolchain = rootDir.resolve("toolchain")
    workingDir = repo
    environment("CARGO_TARGET_DIR", repo.resolve("target/gradle-cargo"))
    environment("CMAKE_TOOLCHAIN_FILE_aarch64_linux_android", toolchain.resolve("ndk.cmake"))
    // fdk-aac's log/log.h, which the NDK hasn't.
    environment("CFLAGS_aarch64_linux_android", "-I${toolchain.resolve("include")}")
    environment("CXXFLAGS_aarch64_linux_android", "-I${toolchain.resolve("include")}")
    commandLine(
        "cargo", "ndk", "-t", "arm64-v8a", "--platform", "30", "-o", nativeLibs,
        "build", "--release", "-p", "carjack-android",
    )
    outputs.dir(nativeLibs)
    // Cargo knows what's changed.
    outputs.upToDateWhen { false }
}
