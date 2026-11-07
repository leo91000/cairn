package build.cairn.app.ui

import build.cairn.app.data.*
import build.cairn.contracts.EngineRetry
import build.cairn.contracts.QuotaFailure
import build.cairn.contracts.TimelineContent
import build.cairn.contracts.UsageLimit
import java.time.LocalDate
import java.time.ZoneOffset
import org.junit.Assert.*
import org.junit.Test

/** Mirrors the web engine-status cases: the server's engine cards are notices, not failures. */
class EngineStatusTest {
    private val retry =
        EngineRetry(
            attempt = 2,
            delayMs = 4000,
            httpStatus = 529,
            maxRetries = 10,
            noResponse = false,
            reason = "overloaded",
        )

    private fun limit(status: String, blocked: Boolean = false, resetsAt: Long? = null) =
        UsageLimit(blocked = blocked, resetsAt = resetsAt, status = status, window = "five_hour")

    private fun notice(code: String, prominent: Boolean = true) =
        TimelineNotice(code = code, prominent = prominent, state = "info")

    @Test
    fun `presents the engine's retries, usage limit and notices without failing`() {
        val latest = presentNotice(notice("engineRetry").copy(engineRetry = retry))
        assertEquals("Claude Code relance la requête · tentative 2/10", latest.title)
        assertEquals(
            "Serveurs surchargés · HTTP 529 · prochaine tentative dans 4 s",
            latest.subtitle,
        )
        assertFalse(latest.failed)
        val recovered =
            presentNotice(notice("engineRetry", false).copy(engineRetry = retry, recovered = true))
        assertEquals(RECOVERED_STATE, recovered.state)
        assertEquals(
            "Aucune réponse de l’API",
            engineRetryDetail(retry.copy(noResponse = true, httpStatus = null, delayMs = null)),
        )

        assertEquals(
            "Limite de 5 heures bientôt atteinte",
            usageLimitTitle(limit("allowed_warning")),
        )
        assertEquals(
            "Limite de 5 heures atteinte · utilisation supplémentaire en cours",
            usageLimitTitle(limit("rejected")),
        )
        val blocked = presentNotice(notice("usageLimit").copy(usageLimit = limit("rejected", true)))
        assertEquals("Limite de 5 heures atteinte", blocked.title)
        assertFalse(blocked.failed)
        assertEquals(
            "La limite d’utilisation ne s’applique plus",
            usageLimitTitle(limit("allowed")),
        )

        val warning =
            presentNotice(
                notice("engineNotice").copy(level = "warning", detail = "Context is almost full.")
            )
        assertEquals("Avertissement de Claude Code", warning.title)
        assertEquals("Context is almost full.", warning.output)
    }

    @Test
    fun `a usage limit stops showing once its window resets`() {
        val now = 1_000_000L
        val applies = notice("usageLimit").copy(usageLimit = limit("rejected", true, now + 60_000))
        assertTrue(applies.shown(now))
        assertFalse(applies.shown(now + 120_000))
        assertFalse(applies.copy(prominent = false).shown(now))
    }

    @Test
    fun `a conversation says how Codex handles the selected speed`() {
        assertEquals(
            "Ce tour passe en vitesse Standard",
            presentNotice(notice("speed").copy(speed = "standard")).title,
        )
        assertEquals(
            "Vitesse non confirmée pour ce tour",
            presentNotice(notice("speed").copy(speed = "unconfirmed")).title,
        )
        assertEquals(
            "Vitesse choisie demandée pour ce tour",
            presentNotice(notice("speed").copy(speed = "confirmed")).title,
        )
    }

    @Test
    fun `the usage banner gives the reset the engine reported on the current failure`() {
        val resetsAt = 1_893_499_200_000L
        val run =
            Run(
                id = "run",
                status = "running",
                accountWaitReason = "Usage exhausted.",
                snapshot =
                    Snapshot(
                        agent =
                            Agent(
                                exclusiveNodes = emptyList(),
                                plugins = emptyList(),
                                id = "00000000-0000-4000-8000-000000000000",
                                name = "",
                                revision = "",
                                createdAt = 0,
                                access =
                                    AccessPolicy(
                                        mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                                        projects = null,
                                        skills = null,
                                        mcps = null,
                                    ),
                                provider = "claude",
                            ),
                        task =
                            build.cairn.contracts.RunTaskSnapshot(
                                agentId = build.cairn.app.data.MAIN_AGENT_ID,
                                id = "00000000-0000-4000-8000-000000000003",
                                name = "Fixture",
                                prompt = "Fixture",
                            ),
                    ),
                taskId = "",
            )
        fun failure(current: Boolean) =
            TimelineCard(
                attempt = 0,
                content =
                    TimelineContent.C(
                        TimelineNotice(
                            code = "failure",
                            prominent = false,
                            quota =
                                QuotaFailure(
                                    current = current,
                                    message = "Your usage allowance is used up for now.",
                                    resetsAt = resetsAt,
                                ),
                            state = "error",
                        )
                    ),
                createdAt = 1,
                id = "notice:1",
                position = 1,
                revision = 1,
                runId = "run",
                source = 1,
                turn = 1,
                updatedAt = 1,
            )
        val quota = chatQuota(run, listOf(failure(true)))!!
        assertEquals("Your usage allowance is used up for now.", quota.message)
        assertEquals(limitReset(resetsAt), quota.reset)
        // A limit from a turn the conversation moved past lends nothing to a later wait.
        assertNull(chatQuota(run, listOf(failure(false)))!!.reset)
    }

    @Test
    fun `reset times name the day once it is not today`() {
        val resetsAt = 1_893_499_200_000L // 2030-01-01T12:00:00Z
        assertEquals("12:00", limitReset(resetsAt, ZoneOffset.UTC, LocalDate.of(2030, 1, 1)))
        assertEquals(
            "mar. 1 janv. 12:00",
            limitReset(resetsAt, ZoneOffset.UTC, LocalDate.of(2029, 12, 31)),
        )
        assertEquals("", limitReset(null))
    }
}
