import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.URI
import java.net.UnknownHostException
import java.util.ArrayDeque
import java.util.Collections
import java.util.IdentityHashMap
import java.util.concurrent.ExecutionException
import javax.net.ssl.SSLHandshakeException
import org.gradle.api.tasks.testing.TestDescriptor
import org.gradle.api.tasks.testing.TestListener
import org.gradle.api.tasks.testing.TestResult

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

val releaseVersion = providers.environmentVariable("CAIRN_ANDROID_VERSION").orElse("0.52.2").get()

require(Regex("(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)").matches(releaseVersion))

val versionParts = releaseVersion.split(".").map { it.toInt() }

require(versionParts[0] in 0..1999 && versionParts[1] in 0..999 && versionParts[2] in 0..999)

val releaseCode =
    100_000_000 + versionParts[0] * 1_000_000 + versionParts[1] * 1000 + versionParts[2]
val releaseKeystore = providers.environmentVariable("CAIRN_ANDROID_KEYSTORE").orNull
val beaconOrigin = providers.environmentVariable("CAIRN_BEACON_ORIGIN").orElse("").get()
val firebaseConfiguration =
    listOf("APP_ID", "PROJECT_ID", "API_KEY", "SENDER_ID").associateWith {
        providers.environmentVariable("CAIRN_ANDROID_FIREBASE_$it").orElse("").get()
    }

require(
    firebaseConfiguration.values.all { it.isEmpty() } ||
        firebaseConfiguration.values.all { it.isNotBlank() }
) {
    "Configure all four public CAIRN_ANDROID_FIREBASE_* values, or none."
}

require(firebaseConfiguration.values.all { Regex("[A-Za-z0-9_:.\\-]*").matches(it) }) {
    "Invalid public Firebase configuration."
}

if (beaconOrigin.isNotEmpty()) {
    val origin = URI(beaconOrigin)
    require(
        origin.host != null &&
            origin.rawUserInfo == null &&
            origin.rawQuery == null &&
            origin.rawFragment == null &&
            origin.rawPath in listOf("", "/") &&
            (origin.scheme == "https" ||
                (origin.scheme == "http" &&
                    origin.host in listOf("localhost", "127.0.0.1", "10.0.2.2")))
    ) {
        "CAIRN_BEACON_ORIGIN must be an HTTPS origin (HTTP only on loopback for development)."
    }
}

require(releaseKeystore == null || beaconOrigin.startsWith("https://")) {
    "Signed distribution requires the fixed HTTPS CAIRN_BEACON_ORIGIN."
}

android {
    namespace = "build.cairn.app"
    compileSdk = 37
    defaultConfig {
        applicationId = "build.cairn.app"
        minSdk = 26
        targetSdk = 37
        versionCode = releaseCode
        versionName = releaseVersion
        buildConfigField(
            "String",
            "BEACON_SERVICE_ORIGIN",
            "\"${beaconOrigin.removeSuffix("/")}\"",
        )
        firebaseConfiguration.forEach { (name, value) ->
            buildConfigField("String", "FIREBASE_$name", "\"$value\"")
        }
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
                storePassword = providers.environmentVariable("CAIRN_ANDROID_STORE_PASSWORD").get()
                keyAlias = "cairn-android"
                keyPassword = providers.environmentVariable("CAIRN_ANDROID_STORE_PASSWORD").get()
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
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.lifecycle:lifecycle-process:2.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0")
    implementation("androidx.navigation:navigation-compose:2.10.1")
    implementation("androidx.work:work-runtime:2.11.2")
    implementation("androidx.datastore:datastore-preferences:1.2.1")
    implementation("androidx.browser:browser:1.10.0")
    implementation("androidx.credentials:credentials:1.6.0")
    implementation("androidx.credentials:credentials-play-services-auth:1.6.0")
    implementation("com.google.android.libraries.identity.googleid:googleid:1.2.1")
    implementation("com.google.firebase:firebase-messaging:25.1.3")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.10.0")
    implementation("com.squareup.okhttp3:okhttp:5.3.2")
    implementation("io.github.webrtc-sdk:android:150.7871.01")
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

    val activityCorpus = rootProject.file("../../tests/fixtures/activity-events.json")
    inputs.file(activityCorpus)
    systemProperty("cairn.activityCorpus", activityCorpus.absolutePath)
    val timelineCorpus = rootProject.file("../../tests/fixtures/timeline-corpus.json")
    inputs.file(timelineCorpus)
    systemProperty("cairn.timelineCorpus", timelineCorpus.absolutePath)
    val pluginFormCorpus = rootProject.file("../../tests/fixtures/plugin-form-values.json")
    inputs.file(pluginFormCorpus)
    inputs.file(rootProject.file("../../tests/fixtures/plugin-form-whitespace.json"))
    systemProperty("cairn.pluginFormCorpus", pluginFormCorpus.absolutePath)
    val pluginSetupCorpus = rootProject.file("../../tests/fixtures/plugin-setup.json")
    inputs.file(pluginSetupCorpus)
    systemProperty("cairn.pluginSetupCorpus", pluginSetupCorpus.absolutePath)
    val diagramCorpus = rootProject.file("../../tests/fixtures/diagrams.json")
    inputs.file(diagramCorpus)
    systemProperty("cairn.diagramCorpus", diagramCorpus.absolutePath)
    // Robolectric's Android 16 shared-memory emulation uses JDK file descriptors.
    jvmArgs(
        "--add-opens=java.base/jdk.internal.access=ALL-UNNAMED",
        "--add-opens=java.base/java.io=ALL-UNNAMED",
    )
    providers.gradleProperty("cairnScreenshotsDir").orNull?.let {
        systemProperty("cairn.screenshots.dir", it)
    }
}

val checkDebugAndroidTestDexSize by
    tasks.registering(Exec::class) {
        dependsOn("packageDebugAndroidTest")
        commandLine(
            "node",
            rootProject.file("../../scripts/check-android-dex-size.mjs"),
            layout.buildDirectory
                .file("outputs/apk/androidTest/debug/app-debug-androidTest.apk")
                .get()
                .asFile,
        )
    }

tasks
    .matching { it.name == "assembleDebugAndroidTest" }
    .configureEach {
        dependsOn(checkDebugAndroidTestDexSize)
    }

// Keep messages, URLs, test names and fixture content out of failure diagnostics.
// Gradle's short exception output omits the HTTP status behind SDK fetch failures.
class AndroidTestFailureSummary : TestListener {
    override fun beforeSuite(suite: TestDescriptor) {}

    override fun afterSuite(suite: TestDescriptor, result: TestResult) {}

    override fun beforeTest(test: TestDescriptor) {}

    override fun afterTest(test: TestDescriptor, result: TestResult) {
        if (result.resultType == TestResult.ResultType.FAILURE) {
            println(summarize(result.exceptions))
        }
    }

    companion object {
        fun summarize(errors: List<Throwable>): String =
            try {
                summarizeKnown(errors)
            } catch (_: Throwable) {
                // A diagnostic must never replace the original test verdict.
                "CAIRN_ANDROID_TEST_FAILURE diagnosticUnavailable=true"
            }

        private fun summarizeKnown(errors: List<Throwable>): String {
            val pending = ArrayDeque<Throwable>()
            val scheduled = Collections.newSetFromMap(IdentityHashMap<Throwable, Boolean>())
            val statuses = sortedSetOf<Int>()
            var causesTruncated = false
            var framesTruncated = false
            var messagesTruncated = false
            var sdkFetch = false
            var io = false
            var timeout = false
            var unknownHost = false
            var tlsHandshake = false
            var connection = false
            val httpStatus = Regex("Server returned HTTP response code: ([1-5][0-9]{2})(?=\\s|$)")

            fun enqueue(error: Throwable) {
                if (error in scheduled) return
                if (scheduled.size == 64) {
                    causesTruncated = true
                    return
                }
                scheduled.add(error)
                pending.add(error)
            }

            for (error in errors.take(65)) enqueue(error)
            causesTruncated = causesTruncated || errors.size > 64
            while (pending.isNotEmpty()) {
                val error = pending.removeFirst()
                val frames = error.stackTrace
                framesTruncated = framesTruncated || frames.size > 256
                sdkFetch =
                    sdkFetch ||
                        frames.take(256).any {
                            it.className ==
                                "org.robolectric.internal.dependency.MavenArtifactFetcher"
                        }
                io = io || error is IOException
                timeout = timeout || error is SocketTimeoutException
                unknownHost = unknownHost || error is UnknownHostException
                tlsHandshake = tlsHandshake || error is SSLHandshakeException
                connection = connection || error is ConnectException
                val message = error.message.orEmpty()
                messagesTruncated = messagesTruncated || message.length > 16_384
                // Only HTTP 100..599 values following the JDK's fixed prefix are retained.
                for (match in httpStatus.findAll(message.take(16_384))) {
                    statuses.add(match.groupValues[1].toInt())
                }
                error.cause?.let { enqueue(it) }
                val suppressed = error.suppressed
                for (other in suppressed.take(65)) enqueue(other)
                causesTruncated = causesTruncated || suppressed.size > 64
            }

            return "CAIRN_ANDROID_TEST_FAILURE sdkFetch=$sdkFetch io=$io " +
                "timeout=$timeout unknownHost=$unknownHost tlsHandshake=$tlsHandshake " +
                "connection=$connection httpStatuses=${statuses.joinToString(",")} " +
                "causesTruncated=$causesTruncated framesTruncated=$framesTruncated " +
                "messagesTruncated=$messagesTruncated"
        }

        fun verify() {
            val fetch =
                IOException(
                    "Server returned HTTP response code: 503 for URL: https://private.invalid/fixture?status=200"
                )
            fetch.stackTrace =
                arrayOf(
                    StackTraceElement(
                        "org.robolectric.internal.dependency.MavenArtifactFetcher",
                        "fetch",
                        "MavenArtifactFetcher.java",
                        130,
                    )
                )
            fetch.addSuppressed(SSLHandshakeException("private fixture"))
            val wrapped = ExecutionException(AssertionError(fetch))
            check(
                summarize(listOf(wrapped)) ==
                    "CAIRN_ANDROID_TEST_FAILURE sdkFetch=true io=true timeout=false unknownHost=false tlsHandshake=true connection=false httpStatuses=503 causesTruncated=false framesTruncated=false messagesTruncated=false"
            ) {
                "Nested SDK failure summary did not match the fixed-field contract"
            }
            check(
                summarize(listOf(AssertionError("private fixture 429"))) ==
                    "CAIRN_ANDROID_TEST_FAILURE sdkFetch=false io=false timeout=false unknownHost=false tlsHandshake=false connection=false httpStatuses= causesTruncated=false framesTruncated=false messagesTruncated=false"
            ) {
                "Unstructured messages must not become diagnostics"
            }
            val statusBoundaries =
                IOException(
                    listOf(99, 100, 599, 600).joinToString("\n") {
                        "Server returned HTTP response code: $it for URL: private fixture"
                    }
                )
            check(summarize(listOf(statusBoundaries)).contains("httpStatuses=100,599 ")) {
                "HTTP statuses must remain in the explicit 100..599 range"
            }
            val cycle = IOException("private fixture")
            val other = IOException("private fixture", cycle)
            cycle.initCause(other)
            check(summarize(listOf(cycle)).contains("causesTruncated=false")) {
                "Cause cycles must be bounded"
            }
            val chain =
                (1..65).fold(null as Throwable?) { cause, _ ->
                    IOException("private fixture", cause)
                }
            check(summarize(listOf(chain!!)).contains("causesTruncated=true")) {
                "Omitted causes must be reported"
            }
            val unreadable =
                object : RuntimeException() {
                    override val message: String
                        get() = error("private fixture")
                }
            check(
                summarize(listOf(unreadable)) ==
                    "CAIRN_ANDROID_TEST_FAILURE diagnosticUnavailable=true"
            ) {
                "Diagnostic errors must not escape or disclose exception text"
            }
        }
    }
}

val checkAndroidTestFailureSummary by tasks.registering {
    group = "verification"
    description = "Verify bounded Android unit failure diagnostics without disclosing messages."
    doLast { AndroidTestFailureSummary.verify() }
}

tasks.withType<Test>().configureEach {
    dependsOn(checkAndroidTestFailureSummary)
    addTestListener(AndroidTestFailureSummary())
}
