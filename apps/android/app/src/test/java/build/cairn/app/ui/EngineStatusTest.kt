package build.cairn.app.ui

import build.cairn.app.data.*
import java.time.LocalDate
import java.time.ZoneOffset
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

/** Mirrors the web engine-status cases: retries, usage limits and notices are not failures. */
class EngineStatusTest {
    private var serial = 0L

    private fun event(type: String, body: JsonObjectBuilder.() -> Unit = {}): RunEvent {
        val id = ++serial
        return RunEvent(
            id,
            id * 1000,
            type,
            "",
            buildJsonObject {
                put("type", type)
                body()
            },
            runId = "run",
        )
    }

    private fun retry(attempt: Int) =
        event("engine.retry") {
            put("attempt", attempt)
            put("maxRetries", 10)
            put("delayMs", 4000)
            put("status", 529)
            put("reason", "overloaded")
        }

    private fun limit(status: String, blocked: Boolean = false) =
        event("provider.limit") {
            put("status", status)
            put("window", "five_hour")
            put("blocked", blocked)
        }

    private fun notice(level: String) =
        event("engine.notice") {
            put("level", level)
            put("message", "Context is almost full.")
        }

    private fun step() =
        event("item.completed") {
            put(
                "item",
                buildJsonObject {
                    put("id", "tool-$serial")
                    put("type", "command_execution")
                    put("command", "ls")
                    put("exit_code", 0)
                },
            )
        }

    private fun notices(events: List<RunEvent>) =
        timelineEntries(events, chat = true).filter { it.notice }.map { it.events.single() }

    @Test
    fun `presents the engine's retries, usage limit and notices without failing`() {
        val retry = presentActivity(retry(2))
        assertEquals("Claude Code relance la requête · tentative 2/10", retry.title)
        assertEquals(
            "Serveurs surchargés · HTTP 529 · prochaine tentative dans 4 s",
            retry.subtitle,
        )
        assertFalse(retry.failed)

        val noResponse =
            presentActivity(
                event("engine.retry") {
                    put("attempt", 1)
                    put("noResponse", buildJsonObject { put("waitedMs", 30000) })
                }
            )
        assertEquals("Aucune réponse de l’API", noResponse.subtitle)

        assertEquals(
            "Limite de 5 heures bientôt atteinte",
            presentActivity(limit("allowed_warning")).title,
        )
        assertEquals(
            "Limite de 5 heures atteinte · utilisation supplémentaire en cours",
            presentActivity(limit("rejected")).title,
        )
        val blocked = presentActivity(limit("rejected", blocked = true))
        assertEquals("Limite de 5 heures atteinte", blocked.title)
        assertFalse(blocked.failed)
        assertEquals(
            "La limite d’utilisation ne s’applique plus",
            presentActivity(limit("allowed")).title,
        )

        val warning = presentActivity(notice("warning"))
        assertEquals("Avertissement de Claude Code", warning.title)
        assertEquals("Context is almost full.", warning.output)
    }

    @Test
    fun `a conversation shows the latest retry until work continues`() {
        val first = retry(1)
        val second = retry(2)
        assertEquals(listOf(second.id), notices(listOf(first, second)).map { it.id })
        assertTrue(notices(listOf(first, second, step())).isEmpty())
        assertTrue(notices(listOf(first, event("turn.completed"))).isEmpty())
    }

    @Test
    fun `a conversation shows the usage limit until it no longer applies or the turn fails`() {
        val warning = limit("allowed_warning")
        assertEquals(listOf(warning.id), notices(listOf(warning, step())).map { it.id })
        assertTrue(notices(listOf(warning, limit("allowed"))).isEmpty())

        val blocked = limit("rejected", blocked = true)
        assertEquals(listOf(blocked.id), notices(listOf(blocked)).map { it.id })
        val failed =
            event("turn.failed") {
                put(
                    "error",
                    buildJsonObject {
                        put("message", "Your usage allowance is used up for now.")
                        put("code", "usage_limit_reached")
                    },
                )
            }
        val shown = notices(listOf(blocked, failed)).map { it.id }
        assertFalse(blocked.id in shown)
    }

    @Test
    fun `informational notices stay in the run log`() {
        val warning = notice("warning")
        val info = notice("info")
        val plain = notice("notice")
        assertEquals(listOf(warning.id), notices(listOf(warning, info, plain)).map { it.id })
        assertEquals(3, timelineEntries(listOf(warning, info, plain)).flatMap { it.events }.size)
    }

    @Test
    fun `the usage banner gives the reset the engine reported`() {
        val resetsAt = 1_893_499_200_000L
        val failed =
            RunEvent(
                1,
                1,
                "turn.failed",
                "",
                buildJsonObject {
                    put(
                        "error",
                        buildJsonObject {
                            put("message", "Your usage allowance is used up for now.")
                            put("code", "usage_limit_reached")
                            put("resetsAt", resetsAt)
                        },
                    )
                },
                runId = "run",
            )
        val run =
            Run(
                "run",
                status = "running",
                accountWaitReason = "Usage exhausted.",
                snapshot = Snapshot(agent = Agent(provider = "claude")),
            )
        val quota = chatQuota(run, listOf(failed))!!
        assertEquals("Your usage allowance is used up for now.", quota.message)
        assertEquals(limitReset(resetsAt), quota.reset)
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
