import io.kjson.JSON
import io.kjson.pointer.JSONPointer
import java.io.File
import net.pwall.json.schema.codegen.CodeGenerator

fun main(args: Array<String>) {
    val generator = CodeGenerator()
    // Only opaque adapter or manifest JSON uses this scalar mapping; business unions stay
    // generated.
    generator.addCustomClassByExtension(
        "x-cairn-json-object",
        true,
        "kotlinx.serialization.json.JsonObject",
    )
    generator.configure(File("boundaries.json"))
    generator.setTemplateDirectory(File("templates"))
    generator.additionalPropertiesOption = CodeGenerator.AdditionalPropertiesOption.STRICT
    generator.baseDirectoryName = args[1]
    generator.basePackageName = "build.cairn.contracts"
    generator.generateAll(JSON.parse(File(args[0]).readText())!!, JSONPointer("/definitions"))
    File(args[1], "build/cairn/contracts")
        .walkTopDown()
        .filter { it.extension == "kt" }
        .forEach {
            it.writeText(
                com.facebook.ktfmt.format.Formatter.format(
                    com.facebook.ktfmt.format.Formatter.KOTLINLANG_FORMAT,
                    it.readText(),
                )
            )
        }
}
