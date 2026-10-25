import com.facebook.ktfmt.format.Formatter
import java.io.File

fun main(args: Array<String>) {
    val file = File(args.single())
    file.writeText(Formatter.format(Formatter.KOTLINLANG_FORMAT, file.readText()))
}
