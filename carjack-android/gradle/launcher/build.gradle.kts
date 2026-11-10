// The home screen Slipstream gives the head unit's display, from carjack-yocto's launcher.
plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.compose)
}

val app = rootDir.resolve("app")

android {
    namespace = "io.github.cfraz89.slipstream.launcher"
    compileSdk { version = release(36) { minorApiLevel = 1 } }
    defaultConfig.minSdk = 33
    buildFeatures.compose = true
    sourceSets["main"].apply {
        manifest.srcFile(app.resolve("launcher/AndroidManifest.xml"))
        kotlin.directories += app.resolve("launcher/src").path
        res.directories += app.resolve("launcher/res").path
    }
}

dependencies {
    implementation(project(":ui"))
    implementation(libs.bundles.compose)
}
