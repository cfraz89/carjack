// Material 3 widgets, theme and glyphs, for both apps' settings screens.
plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.compose)
}

val app = rootDir.resolve("app")

android {
    namespace = "io.github.cfraz89.slipstream.ui"
    compileSdk { version = release(36) { minorApiLevel = 1 } }
    defaultConfig.minSdk = 33
    buildFeatures.compose = true
    sourceSets["main"].apply {
        manifest.srcFile(app.resolve("ui/AndroidManifest.xml"))
        kotlin.directories += app.resolve("ui/src").path
        res.directories += app.resolve("ui/res").path
    }
}

dependencies {
    api(libs.bundles.compose)
}
