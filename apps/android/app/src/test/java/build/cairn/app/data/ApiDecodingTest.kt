package build.cairn.app.data

import java.util.concurrent.Executors
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

@Serializable(with = DecodeProbeSerializer::class) data class DecodeProbe(val thread: Thread)

object DecodeProbeSerializer : KSerializer<DecodeProbe> {
    override val descriptor = PrimitiveSerialDescriptor("DecodeProbe", PrimitiveKind.STRING)

    override fun deserialize(decoder: Decoder): DecodeProbe {
        decoder.decodeString()
        return DecodeProbe(Thread.currentThread())
    }

    override fun serialize(encoder: Encoder, value: DecodeProbe) = error("Read only")
}

class ApiDecodingTest {
    @Test
    fun `reads and writes decode away from the calling UI dispatcher`() {
        Executors.newSingleThreadExecutor().asCoroutineDispatcher().use { ui ->
            MockWebServer().use { server ->
                repeat(2) { server.enqueue(MockResponse().setBody("\"response\"")) }
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                runBlocking(ui) {
                    val caller = Thread.currentThread()
                    val read = api.get<DecodeProbe>("/chats")
                    val write = api.send<DecodeProbe>("POST", "/chats")
                    assertSame(caller, Thread.currentThread())
                    assertNotSame(
                        "GET parsing must not block UI input or frames",
                        caller,
                        read.thread,
                    )
                    assertNotSame(
                        "POST parsing must not block UI input or frames",
                        caller,
                        write.thread,
                    )
                }
            }
        }
    }
}
