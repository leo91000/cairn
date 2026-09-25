package dev.leo.manager.ui

import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import dev.leo.manager.data.AccessPolicy
import dev.leo.manager.data.Agent
import dev.leo.manager.data.Skill
import org.junit.Assert.*
import org.junit.Test

class SkillMentionLogicTest {
    private val project = "11111111-1111-4111-8111-111111111111"
    private val other = "22222222-2222-4222-8222-222222222222"

    @Test
    fun `chat skills follow agent access and the chat project`() {
        val skills =
            listOf(
                Skill("review"),
                Skill("deploy", scope = project),
                Skill("secret", scope = other),
                Skill("broken", valid = false),
                Skill("review", "Projet", scope = project),
            )
        assertEquals(
            listOf("$project/deploy", "$project/review"),
            chatSkills(skills, Agent(), project).map { "${it.scope}/${it.name}" },
        )
        assertEquals(
            listOf("deploy", "review", "secret"),
            chatSkills(skills, Agent(), null).map { it.name },
        )
        val limited =
            Agent(
                access =
                    AccessPolicy(
                        projects = listOf(project),
                        skills = listOf("global/review", "$other/secret"),
                    )
            )
        assertEquals(
            listOf("global/review"),
            chatSkills(skills, limited, "").map { "${it.scope}/${it.name}" },
        )
        assertTrue(chatSkills(skills, null, null).isEmpty())
    }

    @Test
    fun `mentions are detected at the caret like the web composer`() {
        assertEquals(SkillMention(4, 8, "rev"), mentionAt("Use \$rev", 8))
        assertEquals(SkillMention(4, 11, "r"), mentionAt("Use \$review now", 6))
        assertEquals(SkillMention(0, 1, ""), mentionAt("$", 1))
        assertNull(mentionAt("cost a\$5", 8))
        assertNull(mentionAt("echo \$HOME", 7))
        assertNull(mentionAt("\\\$re", 4))
        assertNull(mentionAt("Use \$rev now", 12))
    }

    @Test
    fun `matches are ranked and insertion replaces the token`() {
        val skills =
            listOf("code-review", "review", "reviewer", "preview-docs", "ship").map {
                SkillOption(it, if (it == "ship") "Release to production" else "", "global")
            }
        assertEquals(
            listOf("review", "reviewer", "code-review", "preview-docs"),
            matchSkills(skills, "review").map { it.name },
        )
        assertEquals(listOf("code-review"), matchSkills(skills, "cr").map { it.name })
        assertEquals(listOf("ship"), matchSkills(skills, "production").map { it.name })
        assertEquals(
            listOf("code-review", "preview-docs", "review", "reviewer", "ship"),
            matchSkills(skills, "").map { it.name },
        )
        assertEquals(
            TextFieldValue("Use \$review ", TextRange(12)),
            insertSkill(
                TextFieldValue("Use \$rev", TextRange(8)),
                SkillMention(4, 8, "rev"),
                "review",
            ),
        )
        assertEquals(
            TextFieldValue("Use \$review now", TextRange(12)),
            insertSkill(
                TextFieldValue("Use \$re now", TextRange(7)),
                SkillMention(4, 7, "re"),
                "review",
            ),
        )
    }

    @Test
    fun `only known skills outside code are highlighted`() {
        val text = "Run \$review, not \$HOME or `\$review` or \$reviewer"
        assertEquals(listOf(4..10), mentionRanges(text, setOf("review")))
        assertEquals(1, highlightSkills(text, setOf("review"), SpanStyle()).spanStyles.size)
    }
}
