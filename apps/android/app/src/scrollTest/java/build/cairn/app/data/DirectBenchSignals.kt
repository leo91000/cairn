package build.cairn.app.data

import java.util.concurrent.atomic.AtomicInteger
import kotlinx.serialization.json.*
import okhttp3.ResponseBody
import okio.Buffer
import okio.ForwardingSource
import okio.buffer

/** Observe fixed signal categories without changing or retaining the delivered stream. */
internal class DirectBenchSignals {
    val answers = AtomicInteger()
    val candidates = AtomicInteger()
    val malformed = AtomicInteger()
    val closed = AtomicInteger()

    fun observe(body: ResponseBody): ResponseBody =
        object : ResponseBody() {
            private val observed =
                object : ForwardingSource(body.source()) {
                        private val pending = Buffer()
                        private var oversized = false

                        override fun read(sink: Buffer, byteCount: Long): Long {
                            val offset = sink.size
                            val count = super.read(sink, byteCount)
                            if (count > 0) {
                                sink.copyTo(pending, offset, count)
                                while (true) {
                                    val end = pending.indexOf('\n'.code.toByte())
                                    if (end == -1L) break
                                    if (oversized || end > 16384) {
                                        pending.skip(end + 1)
                                        malformed.incrementAndGet()
                                    } else observeLine(pending.readUtf8LineStrict())
                                    oversized = false
                                }
                                if (pending.size > 16384) {
                                    pending.clear()
                                    oversized = true
                                }
                            }
                            return count
                        }

                        override fun close() {
                            closed.incrementAndGet()
                            pending.clear()
                            super.close()
                        }
                    }
                    .buffer()

            override fun contentType() = body.contentType()

            override fun contentLength() = body.contentLength()

            override fun source() = observed
        }

    private fun observeLine(line: String) {
        if (!line.startsWith("data:")) return
        val kind = runCatching {
            wireJson
                .parseToJsonElement(line.substringAfter(':').trim())
                .jsonObject["kind"]
                ?.jsonPrimitive
                ?.contentOrNull
        }
            .getOrNull()
        when (kind) {
            "answer" -> answers.incrementAndGet()
            "candidate" -> candidates.incrementAndGet()
            else -> malformed.incrementAndGet()
        }
    }

    fun summary() =
        "answers=${answers.get()},candidates=${candidates.get()}," +
            "malformed=${malformed.get()},closed=${closed.get()}"
}
