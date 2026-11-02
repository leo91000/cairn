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
        corpus.getValue("uploads").jsonArray.forEach {
            val upload = json.decodeFromJsonElement<UploadChatAttachmentInput>(it)
            assertEquals(it, json.encodeToJsonElement(upload))
        }
        corpus.getValue("invalidUploads").jsonArray.forEach { value ->
            assertFails(value.toString()) {
                json.decodeFromJsonElement<UploadChatAttachmentInput>(value)
            }
        }
        corpus.getValue("creates").jsonArray.forEach {
            json.decodeFromJsonElement<CreateChatInput>(it)
        }
        corpus.getValue("messages").jsonArray.forEach {
            val message = json.decodeFromJsonElement<ChatMessageInput>(it)
            val encoded = json.encodeToJsonElement(message).jsonObject
            assertEquals(it.jsonObject.getValue("id"), encoded.getValue("id"))
            if (it.jsonObject.containsKey("attachmentIds")) {
                assertEquals(it.jsonObject["attachmentIds"], encoded["attachmentIds"])
            }
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
            it.jsonObject["suspendedByExclusive"]
                ?.takeIf { value -> value != JsonNull }
                ?.let { reservation ->
                    val expected = json.decodeFromJsonElement<ExclusiveReservation>(reservation)
                    assertEquals(expected, chat.suspendedByExclusive)
                    assertEquals(expected.reason, chat.suspendedByExclusive?.reason)
                }
        }
        corpus.getValue("invalidViews").jsonArray.forEach { value ->
            assertFails(value.toString()) { json.decodeFromJsonElement<Chat>(value) }
        }
        corpus.getValue("executions").jsonArray.forEach {
            json.decodeFromJsonElement<ChatExecution>(it)
        }
    }
}
