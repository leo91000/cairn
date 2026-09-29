package dev.leo.manager.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.input.OutputTransformation
import androidx.compose.foundation.text.input.TextFieldBuffer
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.Agent
import dev.leo.manager.data.Skill

// Mirrors `skills::mentions` on the server: `$name` after a boundary, outside code.
private val mentionPattern = Regex("""(?<![\w$\\])\$([a-z0-9][a-z0-9-]{0,63})(?![\w-])""")
private val codePattern = Regex("""```[\s\S]*?```|`[^`\n]*`""")
private val typingPattern = Regex("""(?<![\w$\\])\$([a-z0-9-]*)$""")

data class SkillOption(val name: String, val description: String, val scope: String)

data class SkillMention(val start: Int, val end: Int, val query: String)

/** Skill names highlighted in sent messages of the open conversation. */
val LocalSkillNames = compositionLocalOf { emptySet<String>() }

// Chats receive every valid skill the agent may use in the chat's projects.
fun chatSkills(skills: List<Skill>, agent: Agent?, projectId: String?): List<SkillOption> {
    if (agent == null) return emptyList()
    val projects = agent.access.projects
    val allowed = agent.access.skills
    val byName = linkedMapOf<String, SkillOption>()
    skills
        .filter { skill ->
            skill.valid &&
                (skill.scope == "global" ||
                    if (!projectId.isNullOrEmpty()) skill.scope == projectId
                    else projects == null || skill.scope in projects) &&
                (allowed == null || "${skill.scope}/${skill.name}" in allowed)
        }
        .forEach { skill ->
            // A project skill shadows the global one with the same name in the list.
            if (skill.name !in byName || skill.scope != "global")
                byName[skill.name] = SkillOption(skill.name, skill.description, skill.scope)
        }
    return byName.values.sortedBy { it.name }
}

// The `$query` being typed at the caret, including the rest of the token after it.
fun mentionAt(text: String, caret: Int): SkillMention? {
    val cursor = caret.coerceIn(0, text.length)
    val match = typingPattern.find(text.substring(0, cursor)) ?: return null
    val rest = Regex("^[a-z0-9-]*").find(text.substring(cursor))!!.value
    val next = text.getOrNull(cursor + rest.length)
    if (next != null && (next.isLetterOrDigit() || next == '_' || next == '$')) return null
    return SkillMention(cursor - match.value.length, cursor + rest.length, match.groupValues[1])
}

private fun rank(skill: SkillOption, query: String): Int {
    val name = skill.name
    if (query.isEmpty() || name == query) return 0
    if (name.startsWith(query)) return 1
    if (name.split('-').any { it.startsWith(query) }) return 2
    if (query in name) return 3
    var index = 0
    for (char in name) if (index < query.length && char == query[index]) index++
    if (index == query.length) return 4
    return if (skill.description.contains(query, ignoreCase = true)) 5 else -1
}

fun matchSkills(skills: List<SkillOption>, query: String, limit: Int = 50): List<SkillOption> =
    skills
        .map { it to rank(it, query) }
        .filter { it.second >= 0 }
        .sortedWith(
            compareBy<Pair<SkillOption, Int>> { it.second }
                .thenBy { if (query.isEmpty()) 0 else it.first.name.length }
                .thenBy { it.first.name }
        )
        .take(limit)
        .map { it.first }

fun insertSkill(value: TextFieldValue, mention: SkillMention, name: String): TextFieldValue {
    val after = value.text.substring(mention.end)
    val spaced = after.firstOrNull()?.isWhitespace() == true
    val token = "$$name" + if (spaced) "" else " "
    val caret = mention.start + token.length + if (spaced) 1 else 0
    return TextFieldValue(value.text.substring(0, mention.start) + token + after, TextRange(caret))
}

// Ranges of recognised `$skill` tokens, ignoring code spans and blocks.
fun mentionRanges(text: String, names: Set<String>): List<IntRange> {
    if (names.isEmpty() || '$' !in text) return emptyList()
    val code = codePattern.findAll(text).map { it.range }.toList()
    return mentionPattern
        .findAll(text)
        .filter { match ->
            match.groupValues[1] in names && code.none { match.range.first in it }
        }
        .map { it.range }
        .toList()
}

fun highlightSkills(text: String, names: Set<String>, style: SpanStyle): AnnotatedString =
    buildAnnotatedString {
        append(text)
        mentionRanges(text, names).forEach { addStyle(style, it.first, it.last + 1) }
    }

class SkillMentionHighlight(private val names: Set<String>, private val style: SpanStyle) :
    OutputTransformation {
    override fun TextFieldBuffer.transformOutput() {
        mentionRanges(asCharSequence().toString(), names).forEach {
            addStyle(style, it.first, it.last + 1)
        }
    }

    override fun equals(other: Any?) =
        other is SkillMentionHighlight && other.names == names && other.style == style

    override fun hashCode() = 31 * names.hashCode() + style.hashCode()
}

@Composable
fun skillMentionStyle() =
    SpanStyle(
        color = MaterialTheme.colorScheme.primary,
        background = MaterialTheme.colorScheme.primary.copy(alpha = 0.12f),
        fontWeight = FontWeight.Medium,
    )

@Composable
fun SkillSuggestions(
    skills: List<SkillOption>,
    query: String,
    hasAvailableSkills: Boolean,
    scopeLabel: (String) -> String,
    pick: (SkillOption) -> Unit,
) {
    Column(Modifier.fillMaxWidth().testTag("skill-suggestions")) {
        Text(
            "Skills",
            Modifier.padding(start = 14.dp, top = 8.dp, bottom = 4.dp),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (skills.isEmpty()) {
            Column(
                Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp).semantics {
                    liveRegion = LiveRegionMode.Polite
                },
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    if (hasAvailableSkills) "Aucun skill ne correspond à votre recherche"
                    else "Aucun skill disponible pour cette conversation",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    if (hasAvailableSkills) "Essayez un autre nom de skill."
                    else "Gérez les skills globaux et de projet dans la bibliothèque Skills.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else
            LazyColumn(Modifier.heightIn(max = 232.dp)) {
                items(skills, key = { it.name }) { skill ->
                    Row(
                        Modifier.fillMaxWidth()
                            .heightIn(min = 56.dp)
                            .clip(RoundedCornerShape(12.dp))
                            .clickable(role = Role.Button, onClickLabel = "Insérer le skill") {
                                pick(skill)
                            }
                            .padding(horizontal = 12.dp, vertical = 8.dp)
                            .testTag("skill-suggestion-${skill.name}"),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Surface(
                            Modifier.size(32.dp),
                            shape = RoundedCornerShape(8.dp),
                            color = MaterialTheme.colorScheme.primary.copy(alpha = 0.12f),
                            contentColor = MaterialTheme.colorScheme.primary,
                        ) {
                            Box(contentAlignment = Alignment.Center) {
                                Icon(LeoIcons.Book, null, Modifier.size(18.dp))
                            }
                        }
                        Column(Modifier.weight(1f)) {
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(8.dp),
                            ) {
                                Text(
                                    matchedName(skill.name, query),
                                    Modifier.weight(1f, fill = false),
                                    style =
                                        MaterialTheme.typography.bodyMedium.copy(
                                            fontFamily = FontFamily.Monospace
                                        ),
                                    fontWeight = FontWeight.SemiBold,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                Surface(
                                    shape = RoundedCornerShape(4.dp),
                                    color = MaterialTheme.colorScheme.surfaceContainerHigh,
                                ) {
                                    Text(
                                        scopeLabel(skill.scope),
                                        Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        maxLines = 1,
                                    )
                                }
                            }
                            if (skill.description.isNotBlank())
                                Text(
                                    skill.description,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                        }
                    }
                }
            }
        HorizontalDivider(
            Modifier.padding(top = 4.dp),
            color = MaterialTheme.colorScheme.outlineVariant,
        )
    }
}

@Composable
private fun matchedName(name: String, query: String): AnnotatedString {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val accent = MaterialTheme.colorScheme.primary
    val index = if (query.isEmpty()) -1 else name.indexOf(query)
    return buildAnnotatedString {
        withStyle(SpanStyle(color = muted)) { append('$') }
        if (index < 0) append(name)
        else {
            append(name.substring(0, index))
            withStyle(SpanStyle(color = accent)) { append(query) }
            append(name.substring(index + query.length))
        }
    }
}
