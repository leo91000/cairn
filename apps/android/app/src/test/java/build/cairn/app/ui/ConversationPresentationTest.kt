package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ConversationPresentationTest {
    private val first = ChatMessage("one", text = "Bonjour")
    private val second = ChatMessage("two", text = "Ensuite")

    @Test
    fun `queued Claude message explains the missing account and clears notice when running`() {
        val run =
            Run(
                "run",
                status = "queued",
                chatExecution = ChatExecution("one"),
                accountWaitReason =
                    "Reconnect your Claude Code account in Connections to continue.",
                accountRequired = "claude",
            )
        val chat = Chat("chat", run = run, messages = listOf(first))
        assertEquals("Claude Code", chatWaitNotice(run)!!.account)
        assertEquals(
            "En attente d’un compte Claude Code",
            chatDelivery(chat, emptyList()).sending.single().label,
        )
        assertNull(chatWaitNotice(run.copy(status = "running")))
        assertEquals(
            "Démarrage de l’agent…",
            chatDelivery(chat.copy(run = run.copy(status = "running")), emptyList())
                .sending
                .single()
                .label,
        )
    }

    @Test
    fun `other waiting reasons stay visible without a reconnect action`() {
        val reason = "Waiting for the previous execution to stop before recovery."
        val run =
            Run(
                "run",
                status = "queued",
                accountWaitReason = reason,
                chatExecution = ChatExecution("one"),
            )
        assertEquals(ChatWaitNotice(reason), chatWaitNotice(run))
        assertEquals(
            "En attente de l’agent…",
            chatDelivery(Chat("chat", run = run, messages = listOf(first)), emptyList())
                .sending
                .single()
                .label,
        )
        assertNull(chatWaitNotice(run.copy(accountWaitReason = "")))
        assertNull(chatWaitNotice(run.copy(status = "succeeded")))
    }

    @Test
    fun `idle send is in the transcript but later followups wait`() {
        val delivery = chatDelivery(Chat("chat", messages = listOf(first, second)), emptyList())
        assertEquals(listOf("one"), delivery.sending.map { it.message.id })
        assertEquals(listOf("two"), delivery.queued.map { it.id })
    }

    @Test
    fun `dispatch of the first message does not turn it into a queued followup`() {
        val run = Run("run", status = "running", chatExecution = ChatExecution("one"))
        val delivery =
            chatDelivery(Chat("chat", run = run, messages = listOf(first, second)), emptyList())
        assertEquals("Démarrage de l’agent…", delivery.sending.single().label)
        assertEquals("two", delivery.queued.single().id)
    }

    @Test
    fun `paused and failed runs keep messages waiting`() {
        listOf(Chat("chat", paused = true), Chat("chat", run = Run("r", status = "failed")))
            .forEach {
                val result = chatDelivery(it.copy(messages = listOf(first)), emptyList())
                assertTrue(result.sending.isEmpty())
                assertEquals(first, result.queued.single())
            }
    }

    @Test
    fun `steering and acknowledgements do not duplicate or expose private replies`() {
        val run = Run("run", status = "running")
        val privateAnswer =
            first.copy(id = "private", questionId = "question", mode = "steer", text = "secret")
        val chat =
            Chat("chat", run = run, messages = listOf(first.copy(mode = "steer"), privateAnswer))
        assertEquals(listOf("one"), chatDelivery(chat, emptyList()).sending.map { it.message.id })
        val ack = RunEvent(1, 1, "chat.user", "Bonjour", mapOf("messageId" to JsonPrimitive("one")))
        assertTrue(chatDelivery(chat, listOf(ack), first).sending.isEmpty())
    }

    @Test
    fun `local optimistic send and saved message share one identity`() {
        val chat = Chat("chat", messages = listOf(first))
        assertEquals(1, chatDelivery(chat, emptyList(), first).sending.size)
    }

    @Test
    fun `run outcomes and project revisions survive decoding and task attention grouping`() {
        val run =
            wireJson.decodeFromString<Run>(
                """{"id":"r","status":"succeeded","outcome":{"status":"blocked","reason":"Missing access","evidence":["403"],"reportedAt":10},"workspaces":[{"projectId":"p","path":"/p","revision":"abcdef"}]}"""
            )
        assertEquals("À examiner", taskGroup(Task(), run))
        assertEquals("abcdef", run.workspaces.single().revision)
        assertEquals(listOf("403"), run.outcome!!.evidence)
        val project = wireJson.decodeFromString<Project>("""{"sourceMode":"local"}""")
        assertEquals("local", project.sourceMode)
        assertEquals(
            "Terminées",
            taskGroup(Task(), run.copy(outcome = run.outcome.copy(status = "completed"))),
        )
    }

    @Test
    fun `the header names a background wait, a failure or an interruption`() {
        val run = Run("run", status = "running", startedAt = 1)
        val chat = Chat("chat", agentName = "Cairn", projectName = "Site", run = run)
        assertEquals(
            ConversationState("Cairn · Site", "Cairn travaille"),
            conversationState(chat, "Cairn", null),
        )
        assertEquals(
            ConversationState("Cairn · Site", "Tâche en arrière-plan"),
            conversationState(chat, "Cairn", BackgroundWait(listOf("Build"), 1)),
        )
        assertEquals(
            "2 tâches en arrière-plan",
            conversationState(chat, "Cairn", BackgroundWait(listOf("Build", "Lint"), 1)).live,
        )
        // A wait announced earlier does not outlive the run.
        val failed = chat.copy(run = run.copy(status = "failed"))
        assertEquals(
            ConversationState("Échec · Cairn · Site", null),
            conversationState(failed, "Cairn", BackgroundWait(listOf("Build"), 1)),
        )
        assertEquals(
            "Interrompue · Cairn · Site",
            conversationState(chat.copy(run = run.copy(status = "interrupted")), "Cairn", null)
                .status,
        )
        assertEquals("En pause", conversationState(chat.copy(paused = true), "Cairn", null).status)
        assertEquals(
            "En attente",
            conversationState(chat.copy(run = run.copy(status = "queued")), "Cairn", null).status,
        )
        assertEquals(ConversationState("", null), conversationState(null, "Cairn", null))
    }

    @Test
    fun `a failed run explains why the response stopped`() {
        val run = Run("run", status = "failed")
        assertEquals(
            "La réponse s’est arrêtée avant la fin. Reprenez la conversation pour continuer.",
            conversationError(Chat("chat", run = run)),
        )
        assertEquals(
            "Usage limit reached",
            conversationError(Chat("chat", run = run.copy(error = "Usage limit reached"))),
        )
        // The conversation's own error comes first.
        assertEquals(
            "Upload failed",
            conversationError(
                Chat("chat", run = run.copy(error = "Usage limit reached"), error = "Upload failed")
            ),
        )
        assertNull(conversationError(Chat("chat", run = run.copy(status = "succeeded"))))
        assertNull(conversationError(Chat("chat", run = run.copy(error = " ", status = "running"))))
        assertNull(conversationError(null))
    }
}
