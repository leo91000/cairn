plugins {
    id("com.diffplug.spotless") version "8.10.2"
    kotlin("jvm") version "2.3.10"
    kotlin("plugin.serialization") version "2.3.10"
    application
}

repositories { mavenCentral() }

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.10.0")
    implementation("com.facebook:ktfmt:0.64")
    testImplementation(kotlin("test"))
    implementation("net.pwall.json:json-kotlin-schema-codegen:0.123")
}

application { mainClass.set("GenerateKt") }

sourceSets.test {
    kotlin.srcDir("../../apps/android/app/src/main/java/build/cairn/contracts")
    kotlin.exclude("**/ResumeRunClient.kt")
}

tasks.test {
    val wire = rootProject.file("../../packages/contracts/generated/qualification-wire.json")
    inputs.file(wire)
    systemProperty("cairn.contractWire", wire.absolutePath)
    inputs.file(rootProject.file("../../tests/fixtures/resume-contracts.json"))
    systemProperty(
        "cairn.contractFixtures",
        rootProject.file("../../tests/fixtures/resume-contracts.json").absolutePath,
    )
}

tasks.register<JavaExec>("formatClient") {
    classpath = sourceSets.main.get().runtimeClasspath
    mainClass.set("FormatClientKt")
    args(
        rootProject
            .file("../../apps/android/app/src/main/java/build/cairn/contracts/ResumeRunClient.kt")
            .absolutePath
    )
}

spotless {
    kotlin {
        target("src/**/*.kt")
        ktfmt("0.64").kotlinlangStyle()
    }
    kotlinGradle {
        target("*.gradle.kts")
        ktfmt("0.64").kotlinlangStyle()
    }
}
