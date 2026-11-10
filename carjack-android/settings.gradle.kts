// The Android apps over the carjack core: Slipstream, for any phone, and Motopod, for motopod's
// image. Both take app/src and app/ui. Each module's script is in gradle/<module>/, pointing into
// app/; outputs go to the repo's target/gradle/.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode = RepositoriesMode.FAIL_ON_PROJECT_REPOS
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "carjack-android"

for (module in listOf("ui", "launcher", "slipstream", "motopod")) {
    include(":$module")
    project(":$module").projectDir = file("gradle/$module")
}
