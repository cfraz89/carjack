// Slipstream: the generic app, on the public SDK, for any phone running Shizuku.
import java.security.KeyStore

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose)
}

val app = rootDir.resolve("app")

// Sentry's DSN, kept out of the repo: from the environment (the Justfile loads carjack's .env).
// It replaces the empty one in generic/res, as a build type's resources override main's.
val sentryRes = layout.buildDirectory.dir("generated/sentry-res").get().asFile
sentryRes.resolve("values").mkdirs()
sentryRes.resolve("values/sentry.xml").writeText(
    """
    |<resources>
    |    <string name="sentry_dsn" translatable="false">${System.getenv("SENTRY_DSN").orEmpty()}</string>
    |</resources>
    |""".trimMargin(),
)

// The release key (release.jks, its password in keystore.pass), where there is one, so local
// builds and GitHub releases update each other.
val keys = File(System.getenv("SLIPSTREAM_KEYS") ?: "${System.getProperty("user.home")}/.config/slipstream")
val releaseKey = keys.resolve("release.jks").takeIf { it.exists() }

android {
    namespace = "io.github.cfraz89.slipstream"
    compileSdk { version = release(36) { minorApiLevel = 1 } }
    ndkVersion = "29.0.14206865"
    defaultConfig {
        applicationId = "io.github.cfraz89.slipstream"
        minSdk = 33
        targetSdk = 36
        versionCode = 3
        versionName = "0.3.0"
        ndk.abiFilters += "arm64-v8a"
    }
    buildFeatures {
        compose = true
        aidl = true
    }
    sourceSets["main"].apply {
        manifest.srcFile(app.resolve("generic/AndroidManifest.xml"))
        kotlin.directories += listOf(app.resolve("src").path, app.resolve("src-generic").path)
        aidl.directories += app.resolve("src-generic").path
        res.directories += app.resolve("generic/res").path
        jniLibs.directories += (rootProject.extra["nativeLibs"] as File).path
    }
    sourceSets["debug"].res.directories += sentryRes.path
    sourceSets["release"].res.directories += sentryRes.path
    signingConfigs {
        if (releaseKey != null) {
            create("release") {
                storeFile = releaseKey
                storePassword = keys.resolve("keystore.pass").readText().trim()
                keyPassword = storePassword
                keyAlias = KeyStore.getInstance(releaseKey, storePassword!!.toCharArray())
                    .aliases().nextElement()
            }
        }
    }
    buildTypes {
        release {
            // JNI calls back into Native.Callbacks by name.
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs["debug"]
        }
    }
}

tasks.named("preBuild") { dependsOn(":cargoNdk") }

// The androidx versions ask for SDK 37; both apps compile against 36.1, motopod's.
tasks.matching { it.name.matches(Regex("check.*AarMetadata")) }.configureEach { enabled = false }

dependencies {
    implementation(project(":ui"))
    implementation(project(":launcher"))
    implementation(libs.bundles.compose)

    implementation(files(fileTree(app.resolve("libs/shizuku")) { include("*.aar") }))
    implementation(libs.androidx.annotation.annotation)
    implementation(files(fileTree(app.resolve("libs/sentry")) { include("*.aar", "*.jar") }))
    implementation(libs.androidx.core.core)
    implementation(libs.androidx.lifecycle.lifecycle.common.java8)
    implementation(libs.androidx.lifecycle.lifecycle.process)
}
