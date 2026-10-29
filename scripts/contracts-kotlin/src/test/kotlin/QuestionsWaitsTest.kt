import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.json.*

class QuestionsWaitsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun questionsWaitsAndDelegationRetainTheirWireMeaning() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(root, "../../../tests/fixtures/questions-waits-contracts.json").readText()
                )
                .jsonObject
        corpus.getValue("asks").jsonArray.forEach {
            json.decodeFromJsonElement<AskQuestionsInput>(it)
        }
        corpus.getValue("invalidAsks").jsonArray.forEach {
            assertFails(it.toString()) { json.decodeFromJsonElement<AskQuestionsInput>(it) }
        }
        corpus.getValue("questions").jsonArray.forEach {
            val question = json.decodeFromJsonElement<ChatQuestion>(it)
            assertTrue(question.fields.any { field -> field.secret })
            assertEquals(
                question,
                json.decodeFromJsonElement<ChatQuestion>(json.encodeToJsonElement(question)),
            )
        }
        corpus.getValue("answers").jsonArray.forEach {
            val answer = json.decodeFromJsonElement<QuestionAnswerInput>(it)
            assertEquals(it, json.encodeToJsonElement(answer))
        }
        corpus.getValue("wakeups").jsonArray.forEach {
            val wakeup = json.decodeFromJsonElement<ChatWakeup>(it)
            assertEquals(
                wakeup,
                json.decodeFromJsonElement<ChatWakeup>(json.encodeToJsonElement(wakeup)),
            )
            val encoded = json.encodeToJsonElement(wakeup).jsonObject
            if (encoded.containsKey("observation"))
                assertEquals(
                    40972562294L,
                    encoded
                        .getValue("observation")
                        .jsonObject
                        .getValue("members")
                        .jsonArray[0]
                        .jsonObject
                        .getValue("result")
                        .jsonObject
                        .getValue("pluginResult")
                        .jsonObject
                        .getValue("runId")
                        .jsonPrimitive
                        .long,
                )
        }
        corpus.getValue("delegates").jsonArray.forEach {
            val command = json.decodeFromJsonElement<DelegateTaskInput>(it)
            assertEquals(it, json.encodeToJsonElement(command))
        }
        corpus.getValue("invalidDelegates").jsonArray.forEach {
            assertFails(it.toString()) { json.decodeFromJsonElement<DelegateTaskInput>(it) }
        }
    }
}
