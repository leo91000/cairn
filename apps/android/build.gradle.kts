plugins {
    id("com.diffplug.spotless") version "8.10.2"
    id("com.android.application") version "9.2.1" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.3.10" apply false
    id("org.jetbrains.kotlin.plugin.serialization") version "2.3.10" apply false
}

spotless {
    kotlin {
        target("app/src/**/*.kt")
        ktfmt("0.64").kotlinlangStyle()
    }
    kotlinGradle {
        target("*.gradle.kts", "app/*.gradle.kts")
        ktfmt("0.64").kotlinlangStyle()
    }
}
