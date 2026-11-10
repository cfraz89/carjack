// Motopod: preinstalled in motopod's image and platform-signed, so it gets its permissions and
// hidden APIs without anyone at the screen. Built against motopod's framework and signed with its
// platform key, both from the Android tree by motopod's `just platform-sdk` into platform/
// (or MOTOPOD_PLATFORM). Signed with the image's key, it also installs over the image's copy.
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose)
}

val app = rootDir.resolve("app")
val platform = File(System.getenv("MOTOPOD_PLATFORM") ?: rootDir.resolve("platform").path)

android {
    namespace = "io.github.cfraz89.motopod"
    compileSdk { version = release(36) { minorApiLevel = 1 } }
    ndkVersion = "29.0.14206865"
    defaultConfig {
        applicationId = "io.github.cfraz89.motopod"
        minSdk = 33
        targetSdk = 36
        versionCode = 1
        versionName = "1"
        ndk.abiFilters += "arm64-v8a"
    }
    buildFeatures.compose = true
    sourceSets["main"].apply {
        manifest.srcFile(app.resolve("AndroidManifest.xml"))
        kotlin.directories += listOf(app.resolve("src").path, app.resolve("src-platform").path)
        jniLibs.directories += (rootProject.extra["nativeLibs"] as File).path
    }
    buildTypes {
        release {
            // JNI calls back into Native.Callbacks by name.
            isMinifyEnabled = false
        }
    }
}

tasks.named("preBuild") { dependsOn(":cargoNdk") }

// The androidx versions ask for SDK 37; both apps compile against 36.1, motopod's.
tasks.matching { it.name.matches(Regex("check.*AarMetadata")) }.configureEach { enabled = false }

// The unsigned release APK, signed with the platform key into outputs/Motopod.apk.
val signed = layout.buildDirectory.file("outputs/Motopod.apk")
tasks.register<Exec>("motopodApk") {
    dependsOn("assembleRelease")
    val unsigned = layout.buildDirectory.file("outputs/apk/release/motopod-release-unsigned.apk")
    val apksigner = "${System.getenv("ANDROID_HOME")}/build-tools/36.0.0/apksigner"
    commandLine(
        apksigner, "sign", "--key", platform.resolve("platform.pk8"),
        "--cert", platform.resolve("platform.x509.pem"),
        "--out", signed.get().asFile, unsigned.get().asFile,
    )
    outputs.file(signed)
}

// The framework ahead of the SDK's android.jar on Kotlin's classpath, so that where both have a
// class, the framework's hidden members are seen too.
tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach {
    val rest = libraries.from.toList()
    libraries.setFrom(platform.resolve("framework.jar"), *rest.toTypedArray())
}

dependencies {
    implementation(project(":ui"))
    implementation(libs.bundles.compose)
}
