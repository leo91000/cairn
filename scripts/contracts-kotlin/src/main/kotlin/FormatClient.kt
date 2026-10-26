import com.facebook.ktfmt.format.Formatter
import java.io.File

fun main(args: Array<String>) {
    for (path in args) {
        val file = File(path)
        file.writeText(Formatter.format(Formatter.KOTLINLANG_FORMAT, file.readText()))
    }
}
