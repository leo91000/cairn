import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.json.*

class ConversationsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun conversationLifecycleMessagesAndExecutionDecodeLikeRust() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(root, "../../../tests/fixtures/conversation-contracts.json").readText()
                )
                .jsonObject
        corpus.getValue("creates").jsonArray.forEach {
            json.decodeFromJsonElement<CreateChatInput>(it)
        }
        corpus.getValue("messages").jsonArray.forEach {
            val message = json.decodeFromJsonElement<ChatMessageInput>(it)
            assertEquals(
                message,
                json.decodeFromJsonElement<ChatMessageInput>(json.encodeToJsonElement(message)),
            )
        }
        corpus.getValue("invalidMessages").jsonArray.forEach { value ->
            assertFails(value.toString()) { json.decodeFromJsonElement<ChatMessageInput>(value) }
        }
        corpus.getValue("edits").jsonArray.forEach {
            val edit = json.decodeFromJsonElement<EditChatMessage>(it)
            val encoded = json.encodeToJsonElement(edit).jsonObject
            if (!it.jsonObject.containsKey("speed")) assertFalse(encoded.containsKey("speed"))
        }
        corpus.getValue("views").jsonArray.forEach {
            val chat = json.decodeFromJsonElement<Chat>(it)
            assertEquals(chat, json.decodeFromJsonElement<Chat>(json.encodeToJsonElement(chat)))
        }
        corpus.getValue("invalidViews").jsonArray.forEach { value ->
            assertFails(value.toString()) { json.decodeFromJsonElement<Chat>(value) }
        }
        corpus.getValue("executions").jsonArray.forEach {
            json.decodeFromJsonElement<ChatExecution>(it)
        }
    }
}
