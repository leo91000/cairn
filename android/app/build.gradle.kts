plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

val releaseVersion = providers.environmentVariable("LEO_ANDROID_VERSION").orElse("0.48.2").get()

require(Regex("(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)").matches(releaseVersion))

val versionParts = releaseVersion.split(".").map { it.toInt() }

require(versionParts[0] in 0..1999 && versionParts[1] in 0..999 && versionParts[2] in 0..999)

val releaseCode =
    100_000_000 + versionParts[0] * 1_000_000 + versionParts[1] * 1000 + versionParts[2]
val releaseKeystore = providers.environmentVariable("LEO_ANDROID_KEYSTORE").orNull

android {
    namespace = "dev.leo.manager"
    compileSdk = 37
    defaultConfig {
        applicationId = "dev.leo.manager"
        minSdk = 26
        targetSdk = 37
        versionCode = releaseCode
        versionName = releaseVersion
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    testOptions { unitTests.isIncludeAndroidResources = true }
    sourceSets {
        getByName("test").kotlin.directories.add("src/scrollTest/java")
        getByName("androidTest").kotlin.directories.add("src/scrollTest/java")
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    signingConfigs {
        if (releaseKeystore != null) {
            create("distribution") {
                storeFile = file(releaseKeystore)
                storePassword = providers.environmentVariable("LEO_ANDROID_STORE_PASSWORD").get()
                keyAlias = "leo-android"
                keyPassword = providers.environmentVariable("LEO_ANDROID_STORE_PASSWORD").get()
            }
        }
    }
    buildTypes {
        release {
            if (releaseKeystore != null) signingConfig = signingConfigs.getByName("distribution")
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(composeBom)
    androidTestImplementation(composeBom)
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core:1.7.8")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0")
    implementation("androidx.navigation:navigation-compose:2.10.1")
    implementation("androidx.work:work-runtime:2.11.2")
    implementation("androidx.datastore:datastore-preferences:1.2.1")
    implementation("androidx.browser:browser:1.10.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.10.0")
    implementation("com.squareup.okhttp3:okhttp:5.3.2")
    implementation("io.noties.markwon:core:4.6.2")
    implementation("io.noties.markwon:ext-tables:4.6.2")
    implementation("io.noties.markwon:ext-strikethrough:4.6.2")
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    testImplementation("androidx.work:work-testing:2.11.2")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.17")
    testImplementation("androidx.compose.ui:ui-test-junit4")
    testImplementation("com.squareup.okhttp3:mockwebserver:5.3.2")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
    androidTestImplementation("com.squareup.okhttp3:mockwebserver:5.3.2")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}

tasks.withType<Test>().configureEach {
    maxHeapSize = "512m"
    maxParallelForks = 1
    // Robolectric's Android 16 shared-memory emulation uses JDK file descriptors.
    jvmArgs(
        "--add-opens=java.base/jdk.internal.access=ALL-UNNAMED",
        "--add-opens=java.base/java.io=ALL-UNNAMED",
    )
    providers.gradleProperty("leoScreenshotsDir").orNull?.let {
        systemProperty("leo.screenshots.dir", it)
    }
}
